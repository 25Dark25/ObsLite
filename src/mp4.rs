//! Turns a fragmented MP4 into a regular one, in place. Recordings are written fragmented, so a
//! file left by a crash or a power cut still plays up to its last fragment; on a normal stop it
//! becomes a regular MP4, which every editor opens. The audio and video data stay where they are:
//! a full index (`moov`) is written at the end, and the empty `moov` at the start and the
//! fragments' own indexes (`moof`) become padding (`free`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Converts the finished fragmented MP4 at `path`; a file that is not fragmented is left as it is.
pub fn defragment(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    convert(&mut file, false).map(|_| ())
}

/// Converts a recording whose writing stopped partway (a crash, a power cut), keeping every whole
/// fragment. Returns whether it was fragmented: a finished recording is already regular. Fails
/// with `WouldBlock` while another obs-lite still writes it: on Linux it holds a lock on the file
/// until then; on Windows the file cannot be opened without sharing it while it is open for
/// writing elsewhere.
pub fn repair(path: &Path) -> io::Result<bool> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 1); // FILE_SHARE_READ
    let mut file = options.open(path).map_err(|e| {
        // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION.
        if cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33)) { io::ErrorKind::WouldBlock.into() } else { e }
    })?;
    file.try_lock()?;
    convert(&mut file, true)
}

/// What converting needs of a file besides reading and writing.
trait Storage: Read + Write + Seek {
    fn truncate(&mut self, len: u64) -> io::Result<()>;
    /// Puts what was written so far on the disk. The system may otherwise write the small
    /// in-place changes before the new index at the end, and a power cut between them would
    /// leave a file with no index at all.
    fn sync(&mut self) -> io::Result<()>;
}

impl Storage for File {
    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.set_len(len)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.sync_data()
    }
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("fragmented MP4: {what}"))
}

/// A box's place in the file.
struct Header {
    start: u64,
    size: u64,
    /// 8, or 16 with a 64-bit size.
    head: u64,
    kind: [u8; 4],
}

/// The most read of an index box (`moov`, `moof`): far more than a day of recording needs, and
/// a damaged size does not make obs-lite ask for all the memory there is.
const MAX_INDEX: u64 = 1 << 30;

impl Header {
    fn read(r: &mut (impl Read + Seek), start: u64, end: u64) -> io::Result<Self> {
        r.seek(SeekFrom::Start(start))?;
        let mut head = [0u8; 8];
        r.read_exact(&mut head)?;
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, len) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            // "To the end of the file": obs-lite's recordings have none. What a power cut can
            // leave at the end reads as one (zeros), and an index written after it would be
            // inside it, where no player looks.
            0 => return Err(invalid("box of unknown size")),
            1 => {
                let mut large = [0u8; 8];
                r.read_exact(&mut large)?;
                (u64::from_be_bytes(large), 16)
            }
            n => (u64::from(n), 8),
        };
        if size < len {
            return Err(invalid("bad box size"));
        }
        if start.checked_add(size).is_none_or(|box_end| box_end > end) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "box past the end of the file"));
        }
        Ok(Self { start, size, head: len, kind })
    }

    fn read_body(&self, r: &mut (impl Read + Seek)) -> io::Result<Vec<u8>> {
        let len = self.size - self.head;
        if len > MAX_INDEX {
            return Err(invalid("index too big"));
        }
        r.seek(SeekFrom::Start(self.start + self.head))?;
        let mut body = vec![0; len as usize];
        r.read_exact(&mut body)?;
        Ok(body)
    }
}

/// Whether a fragment (`moof` with its `mfhd` first) starts after the box at `from`, before `to`:
/// then that box is not the unfinished last one but a damaged one with the rest of the
/// recording after it.
fn fragment_after(r: &mut (impl Read + Seek), from: u64, to: u64) -> io::Result<bool> {
    // Past the type of the box at `from`, which may be the last fragment cut short.
    let from = from + 8;
    if from >= to {
        return Ok(false);
    }
    r.seek(SeekFrom::Start(from))?;
    let mut left = to - from;
    let mut window = Vec::new();
    let mut chunk = vec![0; 1 << 16];
    while left > 0 {
        let n = chunk.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        r.read_exact(&mut chunk[..n])?;
        left -= n as u64;
        window.extend_from_slice(&chunk[..n]);
        if window.windows(12).any(|w| &w[..4] == b"moof" && &w[8..] == b"mfhd") {
            return Ok(true);
        }
        // What may be the start of one cut by the end of this chunk.
        window.drain(..window.len().saturating_sub(11));
    }
    Ok(false)
}

/// A box of the index, kept as bytes except for the containers that are rebuilt.
enum Node {
    Leaf([u8; 4], Vec<u8>),
    Container([u8; 4], Vec<Node>),
}

const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];

fn parse(data: &[u8]) -> io::Result<Vec<Node>> {
    parse_within(data, CONTAINERS.len())
}

/// `depth`: how many containers deep it may still go (a real index nests them no deeper than
/// `CONTAINERS` lists them; a damaged one could nest them until the stack runs out).
fn parse_within(mut data: &[u8], depth: usize) -> io::Result<Vec<Node>> {
    let mut nodes = Vec::new();
    while data.len() >= 8 {
        let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
        if size < 8 || size > data.len() {
            return Err(invalid("bad box in the index"));
        }
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let body = &data[8..size];
        nodes.push(if CONTAINERS.contains(&&kind) {
            let depth = depth.checked_sub(1).ok_or_else(|| invalid("index nested too deep"))?;
            Node::Container(kind, parse_within(body, depth)?)
        } else {
            Node::Leaf(kind, body.to_vec())
        });
        data = &data[size..];
    }
    Ok(nodes)
}

fn serialize(nodes: &[Node], out: &mut Vec<u8>) {
    for node in nodes {
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        match node {
            Node::Leaf(kind, body) => {
                out.extend_from_slice(kind);
                out.extend_from_slice(body);
            }
            Node::Container(kind, children) => {
                out.extend_from_slice(kind);
                serialize(children, out);
            }
        }
        let size = (out.len() - start) as u32;
        out[start..start + 4].copy_from_slice(&size.to_be_bytes());
    }
}

fn find<'a>(nodes: &'a [Node], kind: &[u8; 4]) -> Option<&'a Node> {
    nodes.iter().find(|n| matches!(n, Node::Leaf(k, _) | Node::Container(k, _) if k == kind))
}

fn leaf<'a>(nodes: &'a [Node], kind: &[u8; 4]) -> io::Result<&'a [u8]> {
    match find(nodes, kind) {
        Some(Node::Leaf(_, body)) => Ok(body),
        _ => Err(invalid("missing box")),
    }
}

fn children<'a>(nodes: &'a [Node], kind: &[u8; 4]) -> io::Result<&'a [Node]> {
    match find(nodes, kind) {
        Some(Node::Container(_, c)) => Ok(c),
        _ => Err(invalid("missing box")),
    }
}

fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
    data.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| invalid("short box"))
}

fn u64_at(data: &[u8], at: usize) -> io::Result<u64> {
    data.get(at..at + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap())).ok_or_else(|| invalid("short box"))
}

/// `mvhd`, `mdhd` and `tkhd` share a layout: version and flags, two dates (4 or 8 bytes each),
/// `between` bytes (a timescale, or a track id and a reserved word), then the duration.
fn duration_at(body: &[u8], between: usize) -> io::Result<usize> {
    match body.first() {
        Some(1) => Ok(20 + between),
        Some(_) => Ok(12 + between),
        None => Err(invalid("short box")),
    }
}

/// The timescale of an `mvhd` or `mdhd`.
fn timescale(body: &[u8]) -> io::Result<u32> {
    u32_at(body, duration_at(body, 0)?)
}

/// Sets the duration of an `mvhd`/`mdhd` (`between` 4) or `tkhd` (`between` 8), moving a
/// version 0 box to version 1 if the duration does not fit in 32 bits.
fn set_duration(body: &mut Vec<u8>, between: usize, duration: u64) -> io::Result<()> {
    let at = duration_at(body, between)?;
    let wide = body[0] == 1;
    if body.len() < at + if wide { 8 } else { 4 } {
        return Err(invalid("short box"));
    }
    if !wide && duration > u64::from(u32::MAX) {
        let mut v1 = vec![1, body[1], body[2], body[3]];
        v1.extend_from_slice(&u64::from(u32::from_be_bytes(body[4..8].try_into().unwrap())).to_be_bytes());
        v1.extend_from_slice(&u64::from(u32::from_be_bytes(body[8..12].try_into().unwrap())).to_be_bytes());
        v1.extend_from_slice(&body[12..12 + between]);
        v1.extend_from_slice(&[0; 8]);
        v1.extend_from_slice(&body[16 + between..]);
        *body = v1;
    }
    let at = duration_at(body, between)?;
    if body[0] == 1 {
        body[at..at + 8].copy_from_slice(&duration.to_be_bytes());
    } else {
        body[at..at + 4].copy_from_slice(&(duration as u32).to_be_bytes());
    }
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Defaults {
    description: u32,
    duration: u32,
    size: u32,
    flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    size: u32,
    duration: u32,
    sync: bool,
    /// Composition time offset.
    offset: i64,
}

/// A run of one track's samples stored back to back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chunk {
    position: u64,
    samples: u32,
    description: u32,
}

#[derive(Default)]
struct Track {
    id: u32,
    defaults: Defaults,
    samples: Vec<Sample>,
    chunks: Vec<Chunk>,
}

/// A sample is a sync sample unless its flags say "non-sync" (`sample_is_non_sync_sample`).
fn is_sync(flags: u32) -> bool {
    flags & 0x1_0000 == 0
}

/// Reads one `moof` (starting at `start`) into the tracks' samples and chunks. Fails if a
/// sample's data is not all before `end`.
fn read_fragment(moof: &[u8], start: u64, end: u64, tracks: &mut [Track]) -> io::Result<()> {
    let past_end = || invalid("samples past the end of the file");
    // Where the previous track's data ended: the default start of the next one's.
    let mut data_end = start;
    for node in parse(moof)? {
        let Node::Leaf(kind, traf) = node else { continue };
        if &kind != b"traf" {
            continue;
        }
        let traf = parse(&traf)?;
        let tfhd = leaf(&traf, b"tfhd")?;
        let flags = u32_at(tfhd, 0)? & 0xFF_FFFF;
        let id = u32_at(tfhd, 4)?;
        let track = tracks.iter_mut().find(|t| t.id == id).ok_or_else(|| invalid("fragment of an unknown track"))?;
        let mut at = 8;
        let mut field = |present: bool, wide: bool| -> io::Result<Option<u64>> {
            if !present {
                return Ok(None);
            }
            let value = if wide { u64_at(tfhd, at)? } else { u64::from(u32_at(tfhd, at)?) };
            at += if wide { 8 } else { 4 };
            Ok(Some(value))
        };
        // Media Foundation gives every track's data position; GStreamer leaves it to the
        // default: the `moof` for the first track, then where the previous track's data ended.
        let base = match field(flags & 0x1 != 0, true)? {
            Some(base) => base,
            None if flags & 0x2_0000 != 0 => start,
            None => data_end,
        };
        let d = track.defaults;
        let defaults = Defaults {
            description: field(flags & 0x2 != 0, false)?.map_or(d.description, |v| v as u32),
            duration: field(flags & 0x8 != 0, false)?.map_or(d.duration, |v| v as u32),
            size: field(flags & 0x10 != 0, false)?.map_or(d.size, |v| v as u32),
            flags: field(flags & 0x20 != 0, false)?.map_or(d.flags, |v| v as u32),
        };
        let mut position = base;
        for node in &traf {
            let Node::Leaf(kind, trun) = node else { continue };
            if kind != b"trun" {
                continue;
            }
            let version_flags = u32_at(trun, 0)?;
            let (signed, flags) = (version_flags >> 24 == 1, version_flags & 0xFF_FFFF);
            let count = u32_at(trun, 4)?;
            let mut at = 8;
            if flags & 0x1 != 0 {
                position = base.checked_add_signed(i64::from(u32_at(trun, at)? as i32)).ok_or_else(|| invalid("bad data offset"))?;
                at += 4;
            }
            let mut first_flags = None;
            if flags & 0x4 != 0 {
                first_flags = Some(u32_at(trun, at)?);
                at += 4;
            }
            // Checked before reading them, so that a damaged count cannot make billions of them:
            // samples given one by one must all be in the box, and samples all of the default
            // size must all fit in the file.
            let per_sample = [0x100, 0x200, 0x400, 0x800].iter().filter(|&&f| flags & f != 0).count() as u64 * 4;
            if u64::from(count) * per_sample > (trun.len().saturating_sub(at)) as u64 {
                return Err(invalid("short box"));
            }
            if flags & 0x200 == 0
                && count > 0
                && (defaults.size == 0 || u64::from(count) * u64::from(defaults.size) > end.saturating_sub(position))
            {
                return Err(past_end());
            }
            let chunk_start = position;
            for i in 0..count {
                let mut next = |present: bool, default: u32| -> io::Result<u32> {
                    if !present {
                        return Ok(default);
                    }
                    let value = u32_at(trun, at)?;
                    at += 4;
                    Ok(value)
                };
                let duration = next(flags & 0x100 != 0, defaults.duration)?;
                let size = next(flags & 0x200 != 0, defaults.size)?;
                let sample_flags = next(flags & 0x400 != 0, defaults.flags)?;
                let offset = next(flags & 0x800 != 0, 0)?;
                let sample_flags = if i == 0 { first_flags.unwrap_or(sample_flags) } else { sample_flags };
                let offset = if signed { i64::from(offset as i32) } else { i64::from(offset) };
                track.samples.push(Sample { size, duration, sync: is_sync(sample_flags), offset });
                position = position.checked_add(u64::from(size)).filter(|&p| p <= end).ok_or_else(past_end)?;
            }
            if count > 0 {
                track.chunks.push(Chunk { position: chunk_start, samples: count, description: defaults.description.max(1) });
            }
        }
        data_end = position;
    }
    Ok(())
}

fn full_box(kind: &[u8; 4], version: u8, body: &[u8]) -> Node {
    let mut data = vec![version, 0, 0, 0];
    data.extend_from_slice(body);
    Node::Leaf(*kind, data)
}

/// Run-length entries of a sequence.
fn runs<T: PartialEq + Copy>(values: impl Iterator<Item = T>) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((n, last)) if *last == v => *n += 1,
            _ => out.push((1, v)),
        }
    }
    out
}

/// The sample tables that replace a track's empty ones.
fn tables(track: &Track) -> Vec<Node> {
    let samples = &track.samples;
    let mut nodes = Vec::new();
    let mut body = Vec::new();
    let stts = runs(samples.iter().map(|s| s.duration));
    body.extend_from_slice(&(stts.len() as u32).to_be_bytes());
    for (count, delta) in stts {
        body.extend_from_slice(&count.to_be_bytes());
        body.extend_from_slice(&delta.to_be_bytes());
    }
    nodes.push(full_box(b"stts", 0, &body));

    if samples.iter().any(|s| s.offset != 0) {
        let ctts = runs(samples.iter().map(|s| s.offset));
        let mut body = (ctts.len() as u32).to_be_bytes().to_vec();
        for (count, offset) in &ctts {
            body.extend_from_slice(&count.to_be_bytes());
            body.extend_from_slice(&(*offset as u32).to_be_bytes());
        }
        nodes.push(full_box(b"ctts", u8::from(ctts.iter().any(|(_, o)| *o < 0)), &body));
    }

    if samples.iter().any(|s| !s.sync) {
        let sync: Vec<u32> = (1..).zip(samples).filter(|(_, s)| s.sync).map(|(n, _)| n).collect();
        let mut body = (sync.len() as u32).to_be_bytes().to_vec();
        sync.iter().for_each(|n| body.extend_from_slice(&n.to_be_bytes()));
        nodes.push(full_box(b"stss", 0, &body));
    }

    let mut body = Vec::new();
    let first = samples.first().map_or(0, |s| s.size);
    if samples.iter().all(|s| s.size == first) {
        body.extend_from_slice(&first.to_be_bytes());
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes());
    } else {
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&(samples.len() as u32).to_be_bytes());
        samples.iter().for_each(|s| body.extend_from_slice(&s.size.to_be_bytes()));
    }
    nodes.push(full_box(b"stsz", 0, &body));

    let mut entries: Vec<(u32, u32, u32)> = Vec::new();
    for (n, c) in (1..).zip(&track.chunks) {
        if entries.last().is_none_or(|&(_, samples, description)| (samples, description) != (c.samples, c.description)) {
            entries.push((n, c.samples, c.description));
        }
    }
    let mut body = (entries.len() as u32).to_be_bytes().to_vec();
    for (first, samples, description) in entries {
        [first, samples, description].iter().for_each(|v| body.extend_from_slice(&v.to_be_bytes()));
    }
    nodes.push(full_box(b"stsc", 0, &body));

    let mut body = (track.chunks.len() as u32).to_be_bytes().to_vec();
    if track.chunks.iter().any(|c| c.position > u64::from(u32::MAX)) {
        track.chunks.iter().for_each(|c| body.extend_from_slice(&c.position.to_be_bytes()));
        nodes.push(full_box(b"co64", 0, &body));
    } else {
        track.chunks.iter().for_each(|c| body.extend_from_slice(&(c.position as u32).to_be_bytes()));
        nodes.push(full_box(b"stco", 0, &body));
    }
    nodes
}

/// The full `moov` for the fragments read into `tracks`, from the initial (empty) one.
fn full_index(init: Vec<Node>, tracks: &[Track]) -> io::Result<Vec<u8>> {
    let movie_scale = u64::from(timescale(leaf(&init, b"mvhd")?)?.max(1));
    let mut movie_duration = 0;
    let mut moov = Vec::new();
    let mut traks = tracks.iter();
    for node in init {
        match node {
            Node::Leaf(kind, _) if &kind == b"mvex" => {}
            Node::Container(kind, trak) if &kind == b"trak" => {
                let track = traks.next().ok_or_else(|| invalid("track count"))?;
                let media_scale = u64::from(timescale(leaf(children(&trak, b"mdia")?, b"mdhd")?)?.max(1));
                let duration: u64 = track.samples.iter().map(|s| u64::from(s.duration)).sum();
                let in_movie = duration.checked_mul(movie_scale).ok_or_else(|| invalid("duration too long"))?.div_ceil(media_scale);
                movie_duration = movie_duration.max(in_movie);
                moov.push(Node::Container(kind, rebuild_trak(trak, track, duration, in_movie)?));
            }
            other => moov.push(other),
        }
    }
    for node in &mut moov {
        if let Node::Leaf(kind, body) = node
            && kind == b"mvhd"
        {
            set_duration(body, 4, movie_duration)?;
        }
    }
    let mut out = Vec::new();
    serialize(&[Node::Container(*b"moov", moov)], &mut out);
    Ok(out)
}

fn rebuild_trak(trak: Vec<Node>, track: &Track, duration: u64, in_movie: u64) -> io::Result<Vec<Node>> {
    trak.into_iter()
        .map(|node| match node {
            Node::Leaf(kind, mut body) if &kind == b"tkhd" => {
                set_duration(&mut body, 8, in_movie)?;
                Ok(Node::Leaf(kind, body))
            }
            Node::Container(kind, mdia) if &kind == b"mdia" => Ok(Node::Container(
                kind,
                mdia.into_iter()
                    .map(|node| match node {
                        Node::Leaf(kind, mut body) if &kind == b"mdhd" => {
                            set_duration(&mut body, 4, duration)?;
                            Ok(Node::Leaf(kind, body))
                        }
                        Node::Container(kind, minf) if &kind == b"minf" => Ok(Node::Container(
                            kind,
                            minf.into_iter()
                                .map(|node| match node {
                                    Node::Container(kind, stbl) if &kind == b"stbl" => {
                                        let mut stbl: Vec<Node> =
                                            stbl.into_iter().filter(|n| matches!(n, Node::Leaf(k, _) if k == b"stsd")).collect();
                                        stbl.extend(tables(track));
                                        Node::Container(kind, stbl)
                                    }
                                    other => other,
                                })
                                .collect(),
                        )),
                        other => Ok(other),
                    })
                    .collect::<io::Result<_>>()?,
            )),
            other => Ok(other),
        })
        .collect()
}

/// Marks the old indexes as padding: the initial `moov` first, and on the disk before the
/// fragments' (a power cut can leave any of these small writes undone), so the full index at the
/// end is the first `moov` from then on and repairing never takes another one for the initial.
fn pad(file: &mut impl Storage, boxes: &[Header], init: Option<&Header>) -> io::Result<()> {
    if let Some(init) = init {
        file.seek(SeekFrom::Start(init.start + 4))?;
        file.write_all(b"free")?;
        file.sync()?;
    }
    for header in boxes.iter().filter(|h| &h.kind == b"moof") {
        file.seek(SeekFrom::Start(header.start + 4))?;
        file.write_all(b"free")?;
    }
    file.flush()
}

/// Converts in place, returning whether the file was fragmented. With `cut_short`, a file whose
/// writing stopped partway is accepted: its unfinished last box, and its last fragment if its
/// data is not all there, are dropped. Damage before that is refused, leaving the file as it was:
/// what comes after it is still there for a recovery tool. Nothing is written before the file
/// has been read through.
fn convert(file: &mut impl Storage, cut_short: bool) -> io::Result<bool> {
    let len = file.seek(SeekFrom::End(0))?;
    let mut top: Vec<Header> = Vec::new();
    let mut end = 0;
    while end < len {
        match Header::read(file, end, len) {
            // A moov after the fragments that does not parse is an index whose writing stopped
            // partway (a full disk): what follows is left over from the old one.
            Ok(header)
                if cut_short
                    && &header.kind == b"moov"
                    && top.iter().any(|h| &h.kind == b"moof")
                    && header.read_body(file).ok().is_none_or(|b| parse(&b).is_err()) =>
            {
                break;
            }
            Ok(header) => {
                end += header.size;
                top.push(header);
            }
            // The unfinished last box; or what was left over after an index whose writing
            // stopped before the file was cut after it, which may start like no box at all.
            Err(e) if cut_short && !top.is_empty() && matches!(e.kind(), io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData) => break,
            Err(e) => return Err(e),
        }
    }
    if !top.iter().any(|h| &h.kind == b"moof") {
        return Ok(false);
    }
    // Nothing past the last whole box is kept (the new index goes over it, and the file is cut
    // after the index), so it must be what is left of the last box, not a damaged one followed by
    // the rest of the recording.
    if end < len && fragment_after(file, end, len)? {
        return Err(invalid("damaged before its end"));
    }
    // The initial moov comes before the fragments; a moov after them is a full index.
    let first_moof = top.iter().position(|h| &h.kind == b"moof").expect("checked above");
    let init_header = top[..first_moof].iter().find(|h| &h.kind == b"moov");
    // A conversion that was interrupted (obs-lite stopped, or the power went, while marking the
    // padding) already wrote the full index after the fragments: only the padding is left to do,
    // rather than adding a second index made from the fragments not yet marked.
    let indexed = top[first_moof..].iter().any(|h| &h.kind == b"moov" && h.read_body(file).is_ok_and(|b| parse(&b).is_ok()));
    if indexed {
        if end < len {
            file.truncate(end)?;
        }
        pad(file, &top, init_header)?;
        return Ok(true);
    }
    let init_header = init_header.ok_or_else(|| invalid("no moov"))?;
    let init = parse(&init_header.read_body(file)?)?;

    let mut tracks = Vec::new();
    for node in &init {
        if let Node::Container(kind, trak) = node
            && kind == b"trak"
        {
            let tkhd = leaf(trak, b"tkhd")?;
            let id_at = if tkhd.first() == Some(&1) { 20 } else { 12 };
            tracks.push(Track { id: u32_at(tkhd, id_at)?, ..Track::default() });
        }
    }
    if let Ok(mvex) = leaf(&init, b"mvex") {
        for node in parse(mvex)? {
            if let Node::Leaf(kind, trex) = node
                && &kind == b"trex"
            {
                let id = u32_at(&trex, 4)?;
                if let Some(track) = tracks.iter_mut().find(|t| t.id == id) {
                    track.defaults =
                        Defaults { description: u32_at(&trex, 8)?, duration: u32_at(&trex, 12)?, size: u32_at(&trex, 16)?, flags: u32_at(&trex, 20)? };
                }
            }
        }
    }
    let moofs: Vec<&Header> = top.iter().filter(|h| &h.kind == b"moof").collect();
    for (n, header) in moofs.iter().enumerate() {
        let before: Vec<(usize, usize)> = tracks.iter().map(|t| (t.samples.len(), t.chunks.len())).collect();
        let read = header.read_body(file).and_then(|moof| read_fragment(&moof, header.start, end, &mut tracks));
        if let Err(e) = read {
            // Only the last fragment can have been cut short; one before it is damaged.
            if !cut_short || n + 1 < moofs.len() {
                return Err(e);
            }
            for (t, (samples, chunks)) in tracks.iter_mut().zip(before) {
                t.samples.truncate(samples);
                t.chunks.truncate(chunks);
            }
        }
    }
    let index = full_index(init, &tracks)?;

    // The new index goes where the fragment index (`mfra`) was, if it is the last box, or at the
    // end; the old indexes become padding last, so the file plays at every step.
    let at = top.last().filter(|h| &h.kind == b"mfra").map_or(end, |h| h.start);
    file.seek(SeekFrom::Start(at))?;
    file.write_all(&index)?;
    let new_end = at + index.len() as u64;
    if new_end < len {
        file.truncate(new_end)?;
    }
    file.sync()?;
    pad(file, &top, Some(init_header))?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut data = (u32::from(version) << 24 | flags).to_be_bytes().to_vec();
        data.extend_from_slice(body);
        bx(kind, &data)
    }

    fn be(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// An initial `moov` laid out as Media Foundation writes it: `mvhd` version 1, `tkhd` and
    /// `mdhd` version 0, empty sample tables, and `mvex`.
    fn init_moov() -> Vec<u8> {
        let mut mvhd = vec![0; 16];
        mvhd.extend_from_slice(&60_000u32.to_be_bytes());
        mvhd.extend_from_slice(&[0; 88]);
        let trak = |id: u32, scale: u32, handler: &[u8; 4], entry: &[u8; 4]| {
            let tkhd = full(b"tkhd", 0, 1, &[be(&[0, 0, id, 0, 0]), vec![0; 60]].concat());
            let mdhd = full(b"mdhd", 0, 0, &[be(&[0, 0, scale, 0]), vec![0; 4]].concat());
            let hdlr = full(b"hdlr", 0, 0, &[vec![0; 4], handler.to_vec(), vec![0; 13]].concat());
            let stsd = full(b"stsd", 0, 0, &[be(&[1]), bx(entry, &[7; 20])].concat());
            let stbl = bx(
                b"stbl",
                &[
                    stsd,
                    full(b"stts", 0, 0, &be(&[0])),
                    full(b"stsc", 0, 0, &be(&[0])),
                    full(b"stsz", 0, 0, &be(&[0, 0])),
                    full(b"stco", 0, 0, &be(&[0])),
                    full(b"stss", 0, 0, &be(&[0])),
                ]
                .concat(),
            );
            let minf = bx(b"minf", &[full(b"vmhd", 0, 1, &[0; 8]), bx(b"dinf", &full(b"dref", 0, 0, &be(&[0]))), stbl].concat());
            bx(b"trak", &[tkhd, bx(b"mdia", &[mdhd, hdlr, minf].concat())].concat())
        };
        let trex = |id: u32| full(b"trex", 0, 0, &be(&[id, 1, 0, 0, 0]));
        bx(
            b"moov",
            &[
                full(b"mvhd", 1, 0, &mvhd),
                trak(1, 60_000, b"vide", b"avc1"),
                trak(2, 48_000, b"soun", b"mp4a"),
                bx(b"mvex", &[trex(1), trex(2)].concat()),
            ]
            .concat(),
        )
    }

    /// Bytes of a sample, so each can be told apart.
    fn content(track: u32, n: usize, size: u32) -> Vec<u8> {
        vec![(track as usize * 100 + n) as u8; size as usize]
    }

    /// How a fragment says where its data is.
    #[derive(Clone, Copy)]
    enum Style {
        /// Media Foundation: each track gives the absolute position of its data (video first).
        Absolute,
        /// GStreamer: no position; the first track's (audio's) data offset counts from the
        /// `moof`, and the next track's data follows it.
        Default,
    }

    /// One `moof` + `mdat` at `at`, with `video` and `audio` samples (duration, size, flags,
    /// composition offset), numbered from `first`.
    fn fragment(at: usize, video: &[Sample], audio: &[Sample], first: (usize, usize), style: Style) -> Vec<u8> {
        let build = |base: u64| {
            let traf = |id: u32, samples: &[Sample], data_offset: Option<u32>| {
                let tfhd = match style {
                    Style::Absolute => full(b"tfhd", 0, 1, &[be(&[id]), base.to_be_bytes().to_vec(), be(&[0])].concat()),
                    Style::Default => full(b"tfhd", 0, 0, &be(&[id])),
                };
                let mut body = be(&[samples.len() as u32]);
                body.extend(data_offset.map(|o| be(&[o])).unwrap_or_default());
                for s in samples {
                    let flags = if s.sync { 0 } else { 0x1_0000 };
                    body.extend(be(&[s.duration, s.size, flags, s.offset as i32 as u32]));
                }
                let trun_flags = if data_offset.is_some() { 0xF01 } else { 0xF00 };
                bx(b"traf", &[tfhd, full(b"trun", 1, trun_flags, &body)].concat())
            };
            let video_bytes: u32 = video.iter().map(|s| s.size).sum();
            let trafs = match style {
                Style::Absolute => [traf(1, video, Some(0)), traf(2, audio, Some(video_bytes))].concat(),
                // `base` here is the data's distance from the `moof`.
                Style::Default => [traf(2, audio, Some(base as u32)), traf(1, video, None)].concat(),
            };
            bx(b"moof", &[full(b"mfhd", 0, 0, &be(&[1])), trafs].concat())
        };
        let moof = build(0);
        let moof = match style {
            Style::Absolute => build((at + moof.len() + 8) as u64),
            Style::Default => build((moof.len() + 8) as u64),
        };
        let mut mdat = Vec::new();
        let video_data = video.iter().enumerate().flat_map(|(i, s)| content(1, first.0 + i, s.size));
        let audio_data = audio.iter().enumerate().flat_map(|(i, s)| content(2, first.1 + i, s.size));
        match style {
            Style::Absolute => mdat.extend(video_data.chain(audio_data)),
            Style::Default => mdat.extend(audio_data.chain(video_data)),
        }
        [moof, bx(b"mdat", &mdat)].concat()
    }

    fn video(sizes: &[u32], offsets: &[i64]) -> Vec<Sample> {
        sizes.iter().zip(offsets).enumerate().map(|(i, (&size, &offset))| Sample { size, duration: 1000, sync: i == 0, offset }).collect()
    }

    fn audio(count: usize) -> Vec<Sample> {
        vec![Sample { size: 40, duration: 1024, sync: true, offset: 0 }; count]
    }

    /// A fragmented file: three fragments, then a fragment index.
    pub(crate) fn fragmented() -> (Vec<u8>, Vec<Sample>, Vec<Sample>) {
        fragmented_with(init_moov(), &[])
    }

    /// A fragmented file with `init` as its initial index and `after_first` after the first
    /// fragment.
    fn fragmented_with(init: Vec<u8>, after_first: &[u8]) -> (Vec<u8>, Vec<Sample>, Vec<Sample>) {
        let mut file = [bx(b"ftyp", b"mp42\0\0\0\0mp41isom"), init].concat();
        let fragments = [
            (video(&[300, 20, 30], &[0, 2000, 0]), audio(2), Style::Absolute),
            (video(&[310, 25, 35], &[0, 0, 0]), audio(3), Style::Default),
            (video(&[290, 22], &[0, 0]), audio(2), Style::Absolute),
        ];
        let (mut all_video, mut all_audio) = (Vec::new(), Vec::new());
        for (v, a, style) in fragments {
            let f = fragment(file.len(), &v, &a, (all_video.len(), all_audio.len()), style);
            file.extend(f);
            if all_video.is_empty() {
                file.extend_from_slice(after_first);
            }
            all_video.extend(v);
            all_audio.extend(a);
        }
        file.extend(bx(b"mfra", &[0; 24]));
        (file, all_video, all_audio)
    }

    fn top_level(file: &[u8]) -> Vec<(u64, [u8; 4])> {
        let mut cursor = Cursor::new(file);
        let (mut at, mut out) = (0, Vec::new());
        while at < file.len() as u64 {
            let h = Header::read(&mut cursor, at, file.len() as u64).unwrap();
            out.push((h.start, h.kind));
            at += h.size;
        }
        out
    }

    fn table(stbl: &[Node], kind: &[u8; 4]) -> Vec<u32> {
        let body = leaf(stbl, kind).unwrap();
        body[4..].chunks(4).map(|c| u32::from_be_bytes(c.try_into().unwrap())).collect()
    }

    /// Reads a track's samples back through the regular index: bytes, durations, sync and
    /// composition offsets.
    fn read_back(file: &[u8], trak: &[Node]) -> Vec<(Vec<u8>, u32, bool, u32)> {
        let stbl = children(children(children(trak, b"mdia").unwrap(), b"minf").unwrap(), b"stbl").unwrap();
        let stsz = table(stbl, b"stsz");
        let count = stsz[1] as usize;
        let sizes: Vec<u32> = if stsz[0] != 0 { vec![stsz[0]; count] } else { stsz[2..].to_vec() };
        let stco = table(stbl, b"stco");
        let stsc = table(stbl, b"stsc");
        let mut per_chunk = Vec::new();
        for (i, entry) in stsc[1..].chunks(3).enumerate() {
            let last = stsc[1..].chunks(3).nth(i + 1).map_or(stco[0] + 1, |e| e[0]);
            per_chunk.extend((entry[0]..last).map(|_| entry[1]));
        }
        let mut positions = Vec::new();
        for (chunk, &n) in stco[1..].iter().zip(&per_chunk) {
            let mut p = *chunk as usize;
            for _ in 0..n {
                positions.push(p);
                p += sizes[positions.len() - 1] as usize;
            }
        }
        let stts = table(stbl, b"stts");
        let durations: Vec<u32> = stts[1..].chunks(2).flat_map(|e| vec![e[1]; e[0] as usize]).collect();
        let sync: Vec<bool> = match leaf(stbl, b"stss") {
            Ok(_) => {
                let list = table(stbl, b"stss");
                (1..=count as u32).map(|n| list[1..].contains(&n)).collect()
            }
            Err(_) => vec![true; count],
        };
        let offsets: Vec<u32> = match leaf(stbl, b"ctts") {
            Ok(_) => table(stbl, b"ctts")[1..].chunks(2).flat_map(|e| vec![e[1]; e[0] as usize]).collect(),
            Err(_) => vec![0; count],
        };
        (0..count).map(|i| (file[positions[i]..positions[i] + sizes[i] as usize].to_vec(), durations[i], sync[i], offsets[i])).collect()
    }

    impl Storage for Cursor<Vec<u8>> {
        fn truncate(&mut self, len: u64) -> io::Result<()> {
            self.get_mut().truncate(len as usize);
            Ok(())
        }

        fn sync(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A file in memory that logs what is done to it, and can stop working after some writes,
    /// as when obs-lite is stopped in the middle.
    struct Logged {
        file: Cursor<Vec<u8>>,
        log: Vec<&'static str>,
        writes_left: usize,
    }

    impl Read for Logged {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.file.read(buf)
        }
    }

    impl Seek for Logged {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.file.seek(to)
        }
    }

    impl Write for Logged {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.writes_left == 0 {
                return Err(io::Error::other("stopped"));
            }
            self.writes_left -= 1;
            self.log.push(if buf == b"free" { "pad" } else { "index" });
            self.file.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Storage for Logged {
        fn truncate(&mut self, len: u64) -> io::Result<()> {
            self.log.push("truncate");
            self.file.truncate(len)
        }

        fn sync(&mut self) -> io::Result<()> {
            self.log.push("sync");
            Ok(())
        }
    }

    /// `file` converted in memory.
    fn converted(file: Vec<u8>, cut_short: bool) -> io::Result<Vec<u8>> {
        let mut cursor = Cursor::new(file);
        convert(&mut cursor, cut_short)?;
        Ok(cursor.into_inner())
    }

    /// Checks that `file` is a regular MP4 holding exactly `video` and `audio`.
    fn assert_regular(file: &[u8], video: &[Sample], audio: &[Sample]) {
        let kinds: Vec<[u8; 4]> = top_level(file).into_iter().map(|(_, k)| k).collect();
        assert_eq!(kinds.first(), Some(b"ftyp"));
        assert_eq!(kinds.last(), Some(b"moov"), "one full index, at the end");
        assert_eq!(kinds.iter().filter(|k| *k == b"moov").count(), 1);
        assert!(!kinds.iter().any(|k| k == b"moof" || k == b"mfra"), "boxes: {kinds:?}");

        let (at, _) = *top_level(file).last().unwrap();
        let moov = parse(&file[at as usize + 8..]).unwrap();
        assert!(find(&moov, b"mvex").is_none(), "no fragment defaults left");
        let traks: Vec<&[Node]> = moov
            .iter()
            .filter_map(|n| match n {
                Node::Container([b't', b'r', b'a', b'k'], c) => Some(c.as_slice()),
                _ => None,
            })
            .collect();
        assert_eq!(traks.len(), 2);

        let mut longest = 0;
        for (track, expected, trak, scale) in [(1, video, traks[0], 60_000), (2, audio, traks[1], 48_000)] {
            let samples = read_back(file, trak);
            assert_eq!(samples.len(), expected.len(), "track {track}");
            for (i, ((bytes, duration, sync, offset), want)) in samples.iter().zip(expected.iter()).enumerate() {
                assert_eq!(*bytes, content(track, i, want.size), "track {track} sample {i} data");
                assert_eq!((*duration, *sync, i64::from(*offset)), (want.duration, want.sync, want.offset), "track {track} sample {i}");
            }
            let mdhd = leaf(children(trak, b"mdia").unwrap(), b"mdhd").unwrap();
            let total: u64 = expected.iter().map(|s| u64::from(s.duration)).sum();
            assert_eq!(u64::from(u32_at(mdhd, 16).unwrap()), total, "track {track} duration");
            longest = longest.max((total * 60_000).div_ceil(scale));
        }
        // The movie lasts as long as its longest track, in the movie's timescale (60 000).
        assert_eq!(u64_at(leaf(&moov, b"mvhd").unwrap(), 24).unwrap(), longest);
    }

    #[test]
    fn a_fragmented_file_becomes_a_regular_one_with_every_sample_in_place() {
        let (original, video, audio) = fragmented();
        assert_regular(&converted(original, false).unwrap(), &video, &audio);
    }

    #[test]
    fn a_repair_finishes_an_index_whose_writing_was_cut_short() {
        // A fragment index big enough for the full index to fit over it, as a long recording's is.
        let (mut original, video, audio) = fragmented();
        original.truncate(original.len() - 32);
        original.extend(bx(b"mfra", &[0, 0, 0, 3].repeat(1000)));
        let at = top_level(&original).into_iter().find(|(_, k)| k == b"mfra").map(|(at, _)| at as usize).unwrap();
        let index = converted(original.clone(), false).unwrap()[at..].to_vec();
        // Stopped (a crash, a full disk) with half the index written over the old one: a moov
        // that does not parse, with the old index's bytes after it. And stopped between writing
        // the index and cutting the file after it: the whole index, with those bytes after it.
        // Either left the recording unrepaired for good.
        for written in [index.len() / 2, index.len()] {
            let mut file = original.clone();
            file[at..at + written].copy_from_slice(&index[..written]);
            let repaired = converted(file, true).unwrap_or_else(|e| panic!("{written} of {} bytes written: {e}", index.len()));
            assert_regular(&repaired, &video, &audio);
        }
    }

    #[test]
    fn a_file_cut_short_is_repaired_up_to_its_last_whole_fragment() {
        // Writing stopped inside the third fragment's data, or inside its index: no fragment
        // index, and part of the last box missing. The first two fragments hold 6 video and 5
        // audio samples.
        let (original, video, audio) = fragmented();
        let third = top_level(&original).into_iter().filter(|(_, k)| k == b"moof").nth(2).unwrap().0 as usize;
        for cut in [original.len() - 32 - 10, third + 20] {
            let mut file = original.clone();
            file.truncate(cut);
            assert!(converted(file.clone(), false).is_err(), "a finished file must be whole");
            assert_regular(&converted(file, true).unwrap(), &video[..6], &audio[..5]);
        }
    }

    #[test]
    fn a_tail_of_zeros_left_by_a_power_cut_is_cut_off() {
        // Some file systems keep a file's new length but not its last data: zeros, which read as
        // a box "to the end of the file" that the new index would have been inside.
        let (mut file, video, audio) = fragmented();
        file.truncate(file.len() - 32);
        file.extend([0; 4096]);
        assert_regular(&converted(file, true).unwrap(), &video, &audio);
    }

    #[test]
    fn a_damaged_last_fragment_is_dropped() {
        // The third fragment's index reached the disk as its size and zeros.
        let (mut file, video, audio) = fragmented();
        let third = top_level(&file).into_iter().filter(|(_, k)| k == b"moof").nth(2).unwrap().0 as usize;
        file.truncate(file.len() - 32);
        file[third + 8..].fill(0);
        assert_regular(&converted(file, true).unwrap(), &video[..6], &audio[..5]);
    }

    #[test]
    fn damage_before_the_end_leaves_the_file_as_it_was() {
        let (original, _, _) = fragmented();
        let first_mdat = top_level(&original).into_iter().find(|(_, k)| k == b"mdat").unwrap().0 as usize;
        let tfhd = original.windows(4).position(|w| w == b"tfhd").unwrap();
        let mut damaged = Vec::new();
        // A box whose size is gone, then the rest of the recording.
        let mut file = original.clone();
        file[first_mdat..first_mdat + 4].copy_from_slice(&3u32.to_be_bytes());
        damaged.push(file);
        // The first fragment's data placed past the end of the file.
        let mut file = original.clone();
        // After the type: version and flags, the track id, then the data's position.
        file[tfhd + 12..tfhd + 20].copy_from_slice(&(1u64 << 40).to_be_bytes());
        damaged.push(file);
        for (n, file) in damaged.into_iter().enumerate() {
            let mut cursor = Cursor::new(file.clone());
            assert!(convert(&mut cursor, true).is_err(), "case {n}");
            assert!(cursor.into_inner() == file, "case {n}: the file was changed");
        }
    }

    #[test]
    fn a_box_size_that_wraps_around_ends_the_scan() {
        // A 64-bit size that, added to where the box starts, goes past 2^64 back to the start of
        // the file: the scan went round forever, or the file was cut near its start.
        let (mut file, video, audio) = fragmented();
        let at = file.len() - 32;
        file.truncate(at);
        file.extend([&be(&[1])[..], b"mfra", &(u64::MAX - at as u64 + 1).to_be_bytes(), &[0; 16]].concat());
        assert_regular(&converted(file, true).unwrap(), &video, &audio);
    }

    /// The initial index with one box's body changed by `change` (`path`: the containers to it).
    fn init_with(path: &[&[u8; 4]], change: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        fn body_mut<'a>(nodes: &'a mut [Node], path: &[&[u8; 4]]) -> &'a mut Vec<u8> {
            for node in nodes {
                match node {
                    Node::Leaf(kind, body) if path.len() == 1 && kind == path[0] => return body,
                    Node::Container(kind, children) if path.len() > 1 && kind == path[0] => return body_mut(children, &path[1..]),
                    _ => {}
                }
            }
            panic!("no box at {path:?}");
        }
        let mut moov = parse(&init_moov()[8..]).unwrap();
        change(body_mut(&mut moov, path));
        let mut out = Vec::new();
        serialize(&[Node::Container(*b"moov", moov)], &mut out);
        out
    }

    #[test]
    fn a_damaged_index_is_refused_without_crashing() {
        // Each crashed obs-lite (and, as the repair runs at start-up, at every start).
        let nested: Vec<u8> = (0..100_000u32).flat_map(|k| [&(8 * (100_000 - k)).to_be_bytes()[..], b"moov"].concat()).collect();
        let inits = [
            init_with(&[b"mvhd"], Vec::clear),
            init_with(&[b"trak", b"tkhd"], Vec::clear),
            // A timescale but no duration after it.
            init_with(&[b"trak", b"mdia", b"mdhd"], |b| b.truncate(16)),
            nested,
        ];
        for (n, init) in inits.into_iter().enumerate() {
            let (file, _, _) = fragmented_with(init, &[]);
            assert!(converted(file, true).is_err(), "case {n}");
        }
    }

    #[test]
    fn a_damaged_sample_count_is_refused_before_reading_the_samples() {
        // Four billion samples with no fields of their own: about 100 GB of memory asked for.
        let traf = bx(b"traf", &[full(b"tfhd", 0, 0x2_0000, &be(&[1])), full(b"trun", 0, 0, &be(&[u32::MAX]))].concat());
        let moof = [full(b"mfhd", 0, 0, &be(&[1])), traf].concat();
        for size in [0, 100] {
            let mut tracks = [Track { id: 1, defaults: Defaults { size, ..Defaults::default() }, ..Track::default() }];
            assert!(read_fragment(&moof, 0, 1 << 20, &mut tracks).is_err(), "default size {size}");
            assert!(tracks[0].samples.is_empty());
        }
    }

    #[test]
    fn an_index_written_with_64_bit_sizes_is_read() {
        let init = init_moov();
        let wide = [&be(&[1])[..], &init[4..8], &(init.len() as u64 + 8).to_be_bytes(), &init[8..]].concat();
        let (file, video, audio) = fragmented_with(wide, &[]);
        assert_regular(&converted(file, false).unwrap(), &video, &audio);
    }

    #[test]
    fn a_fragment_index_before_other_fragments_is_not_written_over() {
        let (file, video, audio) = fragmented_with(init_moov(), &bx(b"mfra", &[0; 24]));
        let mut file = converted(file, false).unwrap();
        // Left where it is, as padding would be.
        let (at, _) = top_level(&file).into_iter().find(|(_, k)| k == b"mfra").unwrap();
        file[at as usize + 4..at as usize + 8].copy_from_slice(b"free");
        assert_regular(&file, &video, &audio);
    }

    #[test]
    fn a_regular_file_is_left_alone() {
        let regular = [bx(b"ftyp", b"mp42\0\0\0\0mp41isom"), bx(b"mdat", &[1; 50]), bx(b"moov", &[])].concat();
        let mut file = Logged { file: Cursor::new(regular.clone()), log: Vec::new(), writes_left: usize::MAX };
        assert!(!convert(&mut file, true).unwrap());
        assert!(file.log.is_empty(), "untouched: {:?}", file.log);
        assert_eq!(file.file.into_inner(), regular);
    }

    #[test]
    fn the_new_index_is_on_disk_before_the_old_ones_become_padding() {
        let (original, _, _) = fragmented();
        let mut file = Logged { file: Cursor::new(original), log: Vec::new(), writes_left: usize::MAX };
        convert(&mut file, false).unwrap();
        let first_pad = file.log.iter().position(|&op| op == "pad").unwrap();
        assert_eq!(file.log[first_pad - 1], "sync", "log: {:?}", file.log);
        assert_eq!(file.log[0], "index");
    }

    #[test]
    fn a_conversion_stopped_halfway_is_finished_with_one_index() {
        let (original, video, audio) = fragmented();
        // Stopped after the new index and the first padding mark.
        let mut file = Logged { file: Cursor::new(original), log: Vec::new(), writes_left: 2 };
        assert!(convert(&mut file, false).is_err());
        assert_eq!(file.log.iter().filter(|&&op| op == "pad").count(), 1);
        // Repaired at the next start.
        assert_regular(&converted(file.file.into_inner(), true).unwrap(), &video, &audio);
    }

    #[test]
    fn a_recording_still_being_written_is_not_repaired() {
        let (original, _, _) = fragmented();
        let path = std::env::temp_dir().join(format!("obs-lite-busy-{}.mp4", std::process::id()));
        std::fs::write(&path, &original).unwrap();
        // Another obs-lite writing it holds this lock until it finishes the file.
        let writer = File::open(&path).unwrap();
        writer.lock().unwrap();
        let result = repair(&path);
        drop(writer);
        let after = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result.map_err(|e| e.kind()), Err(io::ErrorKind::WouldBlock));
        assert!(after == original, "the file was changed");
    }

    #[cfg(windows)]
    #[test]
    fn a_recording_open_for_writing_elsewhere_is_not_repaired() {
        // Media Foundation takes no lock on the file it writes: being open for writing is the sign.
        let (original, _, _) = fragmented();
        let path = std::env::temp_dir().join(format!("obs-lite-open-{}.mp4", std::process::id()));
        std::fs::write(&path, &original).unwrap();
        let writer = OpenOptions::new().write(true).open(&path).unwrap();
        let result = repair(&path);
        drop(writer);
        let after = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(result.map_err(|e| e.kind()), Err(io::ErrorKind::WouldBlock));
        assert!(after == original, "the file was changed");
    }

    #[test]
    fn a_power_cut_while_padding_does_not_cost_the_index() {
        // The system wrote the initial moov's padding but not every fragment's (a power cut
        // before the disk caught up): the only full index is the one at the end.
        let (original, video, audio) = fragmented();
        let moofs: Vec<u64> = top_level(&original).into_iter().filter(|(_, k)| k == b"moof").map(|(at, _)| at).collect();
        let mut file = converted(original, false).unwrap();
        for &at in &moofs[1..] {
            file[at as usize + 4..at as usize + 8].copy_from_slice(b"moof");
        }
        assert_regular(&converted(file, true).unwrap(), &video, &audio);
    }

    #[test]
    fn the_initial_index_is_padded_first_so_the_full_one_comes_first() {
        // Stopped after the new index and one padding mark: the full index at the end must be the
        // first moov left, or players would take the empty one.
        let (original, _, _) = fragmented();
        let mut file = Logged { file: Cursor::new(original), log: Vec::new(), writes_left: 2 };
        assert!(convert(&mut file, false).is_err());
        let file = file.file.into_inner();
        let kinds: Vec<[u8; 4]> = top_level(&file).into_iter().map(|(_, k)| k).collect();
        assert_eq!(kinds.iter().filter(|k| *k == b"moov").count(), 1, "boxes: {kinds:?}");
        assert_eq!(kinds.last(), Some(b"moov"), "boxes: {kinds:?}");
    }

    #[test]
    fn data_past_4_gib_is_indexed_with_64_bit_offsets() {
        let track = Track {
            samples: vec![Sample { size: 10, duration: 1, sync: true, offset: 0 }; 2],
            chunks: vec![Chunk { position: 100, samples: 1, description: 1 }, Chunk { position: 5_000_000_000, samples: 1, description: 1 }],
            ..Track::default()
        };
        let nodes = tables(&track);
        let Some(Node::Leaf(_, co64)) = find(&nodes, b"co64") else { panic!("no co64") };
        assert_eq!(u64_at(co64, 16).unwrap(), 5_000_000_000);
        assert!(find(&nodes, b"stco").is_none());
    }

    #[test]
    fn a_duration_too_long_for_32_bits_moves_the_box_to_version_1() {
        let mut mdhd = [be(&[0, 1, 2, 48_000, 0]), vec![0x55, 0xC4, 0, 0]].concat();
        set_duration(&mut mdhd, 4, 5_000_000_000).unwrap();
        assert_eq!(mdhd[0], 1);
        assert_eq!((u64_at(&mdhd, 4).unwrap(), u64_at(&mdhd, 12).unwrap(), u32_at(&mdhd, 20).unwrap()), (1, 2, 48_000));
        assert_eq!(u64_at(&mdhd, 24).unwrap(), 5_000_000_000);
        assert_eq!(&mdhd[32..], &[0x55, 0xC4, 0, 0], "language kept");
    }
}
