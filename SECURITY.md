# Security policy

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub private vulnerability reporting](https://github.com/25Dark25/obs-lite/security/advisories/new).
**Do not open a public issue.**

Please include the obs-lite version (or commit), your Windows version, the steps to reproduce and
what an attacker gains. obs-lite is maintained by one person on a best-effort basis, so there is no
guaranteed response time; you will get an answer in the advisory thread.

## Supported versions

Only the latest release receives fixes.

## Scope

obs-lite is a local desktop application. It runs with the privileges of the user who starts it,
installs nothing, and its own code opens no network connections. It reads one input file it does
not fully control, `%APPDATA%\obs-lite\settings.json`, and writes MP4 files to the folder chosen in
*Ajustes*.

In scope:

- Code in this repository and the release files published on its Releases page.
- A crafted `settings.json` that causes memory corruption or code execution.
- Recording something other than the selected source, recording after *Detener Grabación*, or
  writing recordings or settings anywhere other than the configured paths.
- Release files whose signature or checksum does not match, or problems in the CI workflows that
  could let someone alter a release build.

Out of scope:

- An attacker who can already run code as the same Windows user or as an administrator: they can
  read recordings and settings directly.
- That recordings are unencrypted files containing whatever was on screen; that is what the program
  is for.
- Bugs in Windows, GPU drivers or third-party crates, unless obs-lite uses them incorrectly. Report
  those upstream; tell us if obs-lite needs to update or work around them.

## Audit status

obs-lite has not been audited. Most of the code was written with an AI coding assistant and has had
no independent review. It contains a small amount of `unsafe` Rust (five `unsafe` blocks and two
`unsafe impl` for Direct3D/COM interop, in `src/encoder.rs` and `src/capture.rs`). Dependencies are
checked for RustSec advisories and licenses with `cargo deny` on every pull request.
