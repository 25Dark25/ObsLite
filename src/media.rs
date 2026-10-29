//! What every platform's encoder shares: the recording clock, encoded packets, and where they
//! go (the recording file and the replay buffer). The platform supplies the `Muxer` that
//! writes MP4 files.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::i18n::tr;

/// Packet times are in 100 ns units, like Media Foundation's.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// One encoded video frame or block of audio.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    /// 100 ns units since the recording clock started.
    pub time: i64,
    pub duration: i64,
    /// A video keyframe (playback can start here); always true for audio.
    pub key: bool,
}

/// Writes already encoded packets into an MP4 file.
pub trait Muxer: Sized + Send {
    /// How an encoded stream is described to the muxer (codec, size, codec data...).
    type Format: Clone + Send;

    /// Creates the file; packet times are shifted so it starts at `start` (a video keyframe).
    fn create(path: &Path, video: &Self::Format, audio: Option<&Self::Format>, start: i64) -> Result<Self, String>;
    /// Writes a packet; audio from before the file's start is skipped.
    fn write(&self, video: bool, packet: &Packet) -> Result<(), String>;
    fn finish(self) -> Result<(), String>;
}

/// The encoding clock: video and audio times are taken from it. It never pauses: pausing the
/// recording takes the time out of the file only (`Outputs::set_file_paused`), so the replay
/// buffer goes on.
pub struct Clock(Mutex<Instant>);

impl Clock {
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }

    pub fn restart(&self) {
        *self.0.lock().unwrap() = Instant::now();
    }

    /// Time since the last restart.
    pub fn elapsed(&self) -> Duration {
        self.0.lock().unwrap().elapsed()
    }
}

/// Where encoded packets go: the recording file and the replay buffer, each optional. Shared
/// with the app, which turns them on and off while the encoder runs.
pub struct Outputs<M: Muxer> {
    /// The recording asked for, and its file once a keyframe has arrived.
    path: Option<PathBuf>,
    file: Option<M>,
    /// Audio not yet in the file: from the last second, or since the recording was asked for.
    pending_audio: VecDeque<Packet>,
    replay: Option<Ring>,
    video_format: Option<M::Format>,
    audio_format: Option<M::Format>,
    /// The encoder should make its next frame a keyframe, for a recording to start on.
    keyframe_wanted: bool,
    error: Option<String>,
    /// While paused nothing reaches the file, which goes on at the first keyframe after (`resuming`),
    /// its times moved back by the pause; the replay buffer keeps everything.
    paused: bool,
    resuming: bool,
    /// Time taken out of the file by pauses, where the last pause began and ended (its video), the
    /// file's first time and the end of its last video packet (encoder times).
    skipped: i64,
    paused_from: i64,
    resumed_at: i64,
    file_start: i64,
    file_end: i64,
}

impl<M: Muxer> Default for Outputs<M> {
    fn default() -> Self {
        Self {
            path: None,
            file: None,
            pending_audio: VecDeque::new(),
            replay: None,
            video_format: None,
            audio_format: None,
            keyframe_wanted: false,
            error: None,
            paused: false,
            resuming: false,
            skipped: 0,
            paused_from: 0,
            resumed_at: 0,
            file_start: 0,
            file_end: 0,
        }
    }
}

impl<M: Muxer> Outputs<M> {
    /// Starts recording to `path` at the next keyframe, which is asked for right away.
    pub fn start_file(&mut self, path: PathBuf) {
        (self.path, self.file, self.error, self.keyframe_wanted) = (Some(path), None, None, true);
        (self.paused, self.resuming, self.skipped, self.paused_from, self.resumed_at) = (false, false, 0, 0, 0);
    }

    /// Pauses or resumes the recording (not the replay buffer).
    pub fn set_file_paused(&mut self, paused: bool) {
        if self.path.is_none() || paused == self.paused {
            return;
        }
        self.paused = paused;
        if paused {
            self.paused_from = self.file_end;
        } else {
            // The file goes on at a keyframe, asked for now; audio from then on waits for it.
            (self.resuming, self.keyframe_wanted) = (true, true);
            self.pending_audio.clear();
        }
    }

    pub fn file_paused(&self) -> bool {
        self.paused
    }

    /// How long the file is so far, pauses left out.
    pub fn recorded(&self) -> Duration {
        let ticks = if self.file.is_some() { self.file_end - self.file_start - self.skipped } else { 0 };
        Duration::from_nanos(ticks.max(0) as u64 * 100)
    }

    /// The recording asked for has failed (its file could not be created or written, or the
    /// encoder failed): it should be stopped, and finishing it reports why.
    pub fn failed(&self) -> bool {
        self.path.is_some() && self.error.is_some()
    }

    /// Stops recording; the returned file must still be finished (it can take a moment).
    pub fn take_file(&mut self) -> Option<PendingFile<M>> {
        let path = self.path.take()?;
        (self.paused, self.resuming) = (false, false);
        Some(PendingFile { path, file: self.file.take(), error: self.error.take() })
    }

    /// Keeps the last `seconds` of encoded video and audio in memory, or stops doing so.
    pub fn set_replay(&mut self, seconds: Option<u32>) {
        self.replay = seconds.map(|s| Ring::new(s as i64 * TICKS_PER_SECOND));
    }

    /// A copy of the replay buffer, to be saved.
    pub fn replay(&self) -> Result<Replay<M>, String> {
        let empty = || tr!("el búfer de repetición todavía está vacío", "the replay buffer is still empty").to_owned();
        let ring = self.replay.as_ref().filter(|r| !r.video.is_empty()).ok_or_else(empty)?;
        let video_format = self.video_format.clone().ok_or_else(empty)?;
        Ok(Replay {
            video_format,
            audio_format: self.audio_format.clone(),
            video: ring.video.iter().cloned().collect(),
            audio: ring.audio.iter().cloned().collect(),
        })
    }

    /// Set by the encoder before any audio packet.
    pub fn set_audio_format(&mut self, format: Option<M::Format>) {
        self.audio_format = format;
    }

    pub fn take_keyframe_request(&mut self) -> bool {
        std::mem::take(&mut self.keyframe_wanted)
    }

    /// A video packet from the encoder; `video_format` is asked for at the first keyframe,
    /// when the encoder knows its codec data.
    pub fn video(&mut self, packet: Packet, video_format: impl FnOnce() -> Result<M::Format, String>) {
        if packet.key && self.video_format.is_none() {
            match video_format() {
                Ok(format) => self.video_format = Some(format),
                Err(e) => {
                    self.error.get_or_insert(e);
                }
            }
        }
        // A file that could not be created is not tried again at every keyframe.
        if let (Some(path), None, None, true, false, Some(video)) =
            (&self.path, &self.file, &self.error, packet.key, self.paused, &self.video_format)
        {
            match M::create(path, video, self.audio_format.as_ref(), packet.time) {
                Ok(file) => {
                    for audio in self.pending_audio.drain(..) {
                        let _ = file.write(false, &audio);
                    }
                    (self.file, self.file_start, self.file_end, self.resuming) = (Some(file), packet.time, packet.time, false);
                }
                Err(e) => {
                    self.error.get_or_insert(e);
                }
            }
        }
        if self.resuming && packet.key && self.file.is_some() {
            // The pause is taken out: the file goes on from where it stopped.
            self.skipped += packet.time - self.file_end;
            (self.resuming, self.resumed_at) = (false, packet.time);
            for audio in std::mem::take(&mut self.pending_audio) {
                self.write_file(false, &audio);
            }
        }
        if !self.paused && !self.resuming {
            self.write_file(true, &packet);
            self.file_end = self.file_end.max(packet.time + packet.duration);
        }
        if let Some(ring) = &mut self.replay {
            ring.push_video(packet);
        }
    }

    pub fn audio(&mut self, packets: Vec<Packet>) {
        for packet in packets {
            // Audio comes a little after the video of its time: what was made before the pause
            // is recorded even if it comes after it.
            let paused = (self.paused || self.resuming) && packet.time >= self.paused_from;
            match &self.file {
                // Sound made while paused is not recorded.
                _ if self.paused && paused => {}
                Some(_) if !paused => self.write_file(false, &packet),
                // Resuming: kept for the keyframe the file goes on at.
                Some(_) => self.pending_audio.push_back(packet.clone()),
                None => {
                    self.pending_audio.push_back(packet.clone());
                    // Without a recording waiting for its keyframe, a second is plenty.
                    if self.path.is_none() || self.error.is_some() {
                        while self.pending_audio.front().is_some_and(|p| p.time < packet.time - TICKS_PER_SECOND) {
                            self.pending_audio.pop_front();
                        }
                    }
                }
            }
            if let Some(ring) = &mut self.replay {
                ring.push_audio(packet);
            }
        }
    }

    pub fn record_error(&mut self, error: String) {
        self.error.get_or_insert(error);
    }

    /// Writes `packet` to the file, if there is one, with the time pauses took out removed.
    /// Audio from the last pause (coming late) is dropped: it would overlap what came after it.
    fn write_file(&mut self, video: bool, packet: &Packet) {
        let Some(file) = &self.file else { return };
        if !video && (self.paused_from..self.resumed_at).contains(&packet.time) {
            return;
        }
        let time = packet.time - self.skipped;
        let result = if self.skipped == 0 { file.write(video, packet) } else { file.write(video, &Packet { time, ..packet.clone() }) };
        if let Err(e) = result {
            self.error.get_or_insert(e);
        }
    }
}

/// A recording that was stopped, still to be written out.
pub struct PendingFile<M: Muxer> {
    path: PathBuf,
    file: Option<M>,
    error: Option<String>,
}

impl<M: Muxer> PendingFile<M> {
    /// Finishes the file. After an error the file is finished too, so it keeps what was
    /// recorded until then, and the error says where it is.
    pub fn finish(self) -> Result<PathBuf, String> {
        let finished = self.file.map(M::finish);
        match (self.error, finished) {
            (None, Some(result)) => result.map(|()| self.path),
            (None, None) => Err(tr!("no se codificó ningún fotograma", "no frame was encoded").to_owned()),
            (Some(e), Some(Ok(()))) => Err(tr!(
                "{} (lo grabado hasta entonces está en '{}')",
                "{} (what was recorded until then is in '{}')",
                e,
                crate::platform::sys::display_path(&self.path).display()
            )),
            (Some(e), _) => Err(e),
        }
    }
}

/// The last seconds of encoded packets, starting at a video keyframe so they play on their own.
struct Ring {
    /// 100 ns units.
    length: i64,
    video: VecDeque<Packet>,
    audio: VecDeque<Packet>,
}

impl Ring {
    fn new(length: i64) -> Self {
        Self { length, video: VecDeque::new(), audio: VecDeque::new() }
    }

    fn push_video(&mut self, packet: Packet) {
        if self.video.is_empty() && !packet.key {
            return;
        }
        let oldest_needed = packet.time - self.length;
        self.video.push_back(packet);
        // Drop whole keyframe intervals while the next keyframe still covers `length`.
        while let Some(next) = self.video.iter().skip(1).position(|p| p.key).map(|i| i + 1)
            && self.video[next].time <= oldest_needed
        {
            self.video.drain(..next);
        }
        self.trim_audio();
    }

    fn push_audio(&mut self, packet: Packet) {
        self.audio.push_back(packet);
        self.trim_audio();
    }

    /// Audio from before the first video frame is of no use, nor is audio older than `length`.
    fn trim_audio(&mut self) {
        let start = match (self.video.front(), self.audio.back()) {
            (Some(video), _) => video.time,
            (None, Some(audio)) => audio.time - self.length,
            (None, None) => return,
        };
        while self.audio.front().is_some_and(|p| p.time < start) {
            self.audio.pop_front();
        }
    }
}

/// A copy of the replay buffer.
pub struct Replay<M: Muxer> {
    video_format: M::Format,
    audio_format: Option<M::Format>,
    video: Vec<Packet>,
    audio: Vec<Packet>,
}

impl<M: Muxer> Replay<M> {
    pub fn save(self, path: PathBuf) -> Result<PathBuf, String> {
        let start = self.video[0].time;
        let file = M::create(&path, &self.video_format, self.audio_format.as_ref(), start)?;
        // Interleaved by time, the order a player reads them in.
        let (mut audio, mut video) = (self.audio.iter().peekable(), self.video.iter().peekable());
        loop {
            let is_video = match (audio.peek(), video.peek()) {
                (Some(a), Some(v)) => v.time <= a.time,
                (Some(_), None) => false,
                (None, Some(_)) => true,
                (None, None) => break,
            };
            let packet = if is_video { video.next() } else { audio.next() };
            if let Err(e) = file.write(is_video, packet.expect("peeked")) {
                // Still finished, so the file is not left half written.
                let _ = file.finish();
                return Err(e);
            }
        }
        file.finish()?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::thread;

    use super::*;

    fn packet(time: i64, key: bool) -> Packet {
        Packet { data: vec![0; 4], time, duration: 0, key }
    }

    /// 30 fps video with a keyframe every 2 s, and audio every 1024 samples, for `seconds`.
    fn fill(ring: &mut Ring, seconds: i64) {
        let audio_step = 1024 * TICKS_PER_SECOND / 48_000;
        let mut audio = 0;
        for n in 0..seconds * 30 {
            let time = n * TICKS_PER_SECOND / 30;
            while audio <= time {
                ring.push_audio(packet(audio, true));
                audio += audio_step;
            }
            ring.push_video(packet(time, n % 60 == 0));
        }
    }

    #[test]
    fn replay_keeps_the_last_seconds_from_a_keyframe() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        fill(&mut ring, 60);
        let (first, newest) = (&ring.video[0], ring.video.back().unwrap());
        assert!(first.key, "the replay must start on a keyframe");
        let covered = newest.time - first.time;
        assert!(covered >= 20 * TICKS_PER_SECOND, "only {covered} covered");
        assert!(covered < 22 * TICKS_PER_SECOND, "{covered} kept: more than one keyframe interval too many");
        assert!(ring.audio[0].time >= first.time, "audio from before the first frame was kept");
        assert!(ring.audio[0].time - first.time < TICKS_PER_SECOND / 40, "audio at the start is missing");
    }

    #[test]
    fn replay_shorter_than_its_length_keeps_everything() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        fill(&mut ring, 5);
        assert_eq!(ring.video.len(), 150);
        assert_eq!(ring.video[0].time, 0);
    }

    #[test]
    fn replay_starts_at_the_first_keyframe() {
        let mut ring = Ring::new(20 * TICKS_PER_SECOND);
        ring.push_video(packet(0, false));
        ring.push_video(packet(1, false));
        assert!(ring.video.is_empty());
        ring.push_video(packet(2, true));
        assert_eq!(ring.video.len(), 1);
    }

    /// Records what would be written, as (video, time relative to the file's start).
    struct FakeMuxer {
        start: i64,
        written: Arc<Mutex<Vec<(bool, i64)>>>,
    }

    impl Muxer for FakeMuxer {
        type Format = Arc<Mutex<Vec<(bool, i64)>>>;

        fn create(_: &Path, video: &Self::Format, _: Option<&Self::Format>, start: i64) -> Result<Self, String> {
            Ok(Self { start, written: video.clone() })
        }

        fn write(&self, video: bool, packet: &Packet) -> Result<(), String> {
            let time = packet.time - self.start;
            if time >= 0 {
                self.written.lock().unwrap().push((video, time));
            }
            Ok(())
        }

        fn finish(self) -> Result<(), String> {
            Ok(())
        }
    }

    /// What a `Broken` muxer was asked to do, and what fails.
    #[derive(Default)]
    struct Calls {
        creates: usize,
        finished: bool,
        fail_create: bool,
        fail_write: bool,
    }

    /// A muxer whose file cannot be created or written, as set in its `Calls`.
    struct Broken(Arc<Mutex<Calls>>);

    impl Muxer for Broken {
        type Format = Arc<Mutex<Calls>>;

        fn create(_: &Path, calls: &Self::Format, _: Option<&Self::Format>, _: i64) -> Result<Self, String> {
            let mut c = calls.lock().unwrap();
            c.creates += 1;
            if c.fail_create { Err("cannot create".into()) } else { Ok(Self(calls.clone())) }
        }

        fn write(&self, _: bool, _: &Packet) -> Result<(), String> {
            if self.0.lock().unwrap().fail_write { Err("disk full".into()) } else { Ok(()) }
        }

        fn finish(self) -> Result<(), String> {
            self.0.lock().unwrap().finished = true;
            Ok(())
        }
    }

    #[test]
    fn a_file_that_cannot_be_created_fails_the_recording_once() {
        let calls = Arc::new(Mutex::new(Calls { fail_create: true, ..Calls::default() }));
        let mut outputs = Outputs::<Broken>::default();
        outputs.start_file(PathBuf::from("rec.mp4"));
        outputs.video(packet(0, true), || Ok(calls.clone()));
        assert!(outputs.failed(), "the app must hear of it while recording");
        // 10 s: a packet of each every 0.1 s, a keyframe every second.
        for n in 1..100 {
            outputs.audio(vec![packet(n * TICKS_PER_SECOND / 10, true)]);
            outputs.video(packet(n * TICKS_PER_SECOND / 10, n % 10 == 0), || unreachable!());
        }
        assert_eq!(calls.lock().unwrap().creates, 1, "not tried again at every keyframe");
        assert!(outputs.pending_audio.len() < 20, "audio is not kept for a file that will not come");
        assert!(outputs.take_file().unwrap().finish().is_err());
    }

    #[test]
    fn a_recording_that_fails_is_still_finished() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut outputs = Outputs::<Broken>::default();
        outputs.start_file(PathBuf::from("rec.mp4"));
        outputs.video(packet(0, true), || Ok(calls.clone()));
        assert!(!outputs.failed());
        calls.lock().unwrap().fail_write = true;
        outputs.video(packet(10, false), || unreachable!());
        assert!(outputs.failed());
        let error = outputs.take_file().unwrap().finish().unwrap_err();
        assert!(calls.lock().unwrap().finished, "the file keeps what was recorded until then");
        assert!(error.contains("disk full") && error.contains("rec.mp4"), "{error}");
    }

    #[test]
    fn a_replay_that_fails_to_write_is_still_finished() {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let mut outputs = Outputs::<Broken>::default();
        outputs.set_replay(Some(20));
        outputs.video(packet(0, true), || Ok(calls.clone()));
        calls.lock().unwrap().fail_write = true;
        assert!(outputs.replay().unwrap().save(PathBuf::from("replay.mp4")).is_err());
        assert!(calls.lock().unwrap().finished);
    }

    #[test]
    fn a_recording_starts_at_the_next_keyframe_with_the_audio_since_then() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut outputs = Outputs::<FakeMuxer>::default();
        // Running for the replay buffer before the recording is asked for.
        outputs.video(packet(0, true), || Ok(written.clone()));
        outputs.audio(vec![packet(5, true)]);
        outputs.start_file(PathBuf::from("rec.mp4"));
        assert!(outputs.take_keyframe_request(), "a new recording must ask for a keyframe");
        assert!(!outputs.take_keyframe_request(), "the request is taken once");
        outputs.audio(vec![packet(15, true)]);
        outputs.video(packet(10, false), || unreachable!());
        outputs.video(packet(20, true), || unreachable!());
        outputs.audio(vec![packet(25, true)]);
        // Nothing before the keyframe at 20 reaches the file (the audio at 5 and 15 is older);
        // the rest is written relative to it.
        assert_eq!(*written.lock().unwrap(), vec![(true, 0), (false, 5)]);
        let file = outputs.take_file().expect("a recording was started");
        assert_eq!(file.finish().unwrap(), PathBuf::from("rec.mp4"));
    }

    #[test]
    fn a_recording_without_frames_is_an_error() {
        let mut outputs = Outputs::<FakeMuxer>::default();
        outputs.start_file(PathBuf::from("rec.mp4"));
        assert!(outputs.take_file().unwrap().finish().is_err());
        assert!(outputs.take_file().is_none(), "the file is handed out once");
    }

    #[test]
    fn a_saved_replay_is_interleaved_by_time() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut outputs = Outputs::<FakeMuxer>::default();
        outputs.set_replay(Some(20));
        assert!(outputs.replay().is_err(), "nothing to save yet");
        outputs.video(packet(100, true), || Ok(written.clone()));
        outputs.audio(vec![packet(90, true), packet(110, true)]);
        outputs.video(packet(120, false), || unreachable!());
        let replay = outputs.replay().unwrap();
        thread::spawn(move || replay.save(PathBuf::from("replay.mp4")).unwrap()).join().unwrap();
        assert_eq!(*written.lock().unwrap(), vec![(true, 0), (false, 10), (true, 20)]);
    }

    /// 30 fps video (a keyframe every 2 s, or when asked for) and audio every 1/30 s, from `from`
    /// to `to` seconds, given to `outputs`.
    fn feed(outputs: &mut Outputs<FakeMuxer>, written: &Arc<Mutex<Vec<(bool, i64)>>>, from: i64, to: i64) {
        let step = TICKS_PER_SECOND / 30;
        for n in from * 30..to * 30 {
            outputs.audio(vec![Packet { duration: step, ..packet(n * step, true) }]);
            let key = n % 60 == 0 || outputs.take_keyframe_request();
            outputs.video(Packet { duration: step, ..packet(n * step, key) }, || Ok(written.clone()));
        }
    }

    #[test]
    fn audio_made_before_a_pause_and_coming_after_it_is_recorded() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut outputs = Outputs::<FakeMuxer>::default();
        outputs.start_file(PathBuf::from("rec.mp4"));
        let step = TICKS_PER_SECOND / 30;
        outputs.video(Packet { duration: step, ..packet(0, true) }, || Ok(written.clone()));
        outputs.video(Packet { duration: step, ..packet(step, false) }, || unreachable!());
        outputs.set_file_paused(true);
        // Made before the video stopped, and after.
        outputs.audio(vec![packet(step, true), packet(2 * step, true)]);
        outputs.set_file_paused(false);
        // Late, from the pause, and then the file goes on.
        outputs.audio(vec![packet(3 * step, true)]);
        outputs.video(Packet { duration: step, ..packet(10 * step, true) }, || unreachable!());
        outputs.audio(vec![packet(5 * step, true), packet(10 * step, true)]);
        let audio: Vec<i64> = written.lock().unwrap().iter().filter(|(v, _)| !*v).map(|(_, t)| *t).collect();
        assert_eq!(audio, [step, 2 * step], "before the pause, and where the file goes on");
    }

    #[test]
    fn a_pause_is_left_out_of_the_file_but_not_out_of_the_replay_buffer() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let mut outputs = Outputs::<FakeMuxer>::default();
        outputs.set_replay(Some(20));
        outputs.start_file(PathBuf::from("rec.mp4"));
        feed(&mut outputs, &written, 0, 2);
        outputs.set_file_paused(true);
        assert!(outputs.file_paused());
        feed(&mut outputs, &written, 2, 5);
        let frames = |n: i64| Duration::from_nanos((n * (TICKS_PER_SECOND / 30) * 100) as u64);
        assert_eq!(outputs.recorded(), frames(60), "the timer stops while paused");
        // At 5 s, between keyframes: the file goes on at the one asked for.
        outputs.set_file_paused(false);
        feed(&mut outputs, &written, 5, 7);
        assert_eq!(outputs.recorded(), frames(120));
        let written = written.lock().unwrap();
        let video: Vec<i64> = written.iter().filter(|(v, _)| *v).map(|(_, t)| *t).collect();
        let audio: Vec<i64> = written.iter().filter(|(v, _)| !*v).map(|(_, t)| *t).collect();
        // 4 s of frames, one after the other with no gap where the pause was, in both streams.
        let step = TICKS_PER_SECOND / 30;
        assert_eq!(video, (0..120).map(|n| n * step).collect::<Vec<_>>());
        assert!(audio.windows(2).all(|w| w[1] - w[0] == step), "a gap or overlap in the audio");
        assert!(*audio.last().unwrap() >= 119 * step - step);
        // The replay buffer has all 7 s, from a keyframe.
        let replay = outputs.replay().unwrap();
        assert_eq!(replay.video.len(), 210);
    }
}
