# Releasing

Releases are tagged, built and signed by the maintainer. Replace `X.Y.Z` with the version.

1. **Prepare** in a pull request: set `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock`
   follows, and in `CHANGELOG.md` rename `[Unreleased]` to `[X.Y.Z] - YYYY-MM-DD`, add an empty
   `[Unreleased]` above it and update the links at the bottom. Run the manual tests in
   [TESTING.md](TESTING.md) and record the results there. Merge.

2. **Tag** the merge commit with a signed tag and push it:

   ```sh
   git switch main && git pull
   git tag -s vX.Y.Z -m "obs-lite X.Y.Z"
   git push origin vX.Y.Z
   ```

3. **Build** the zip: *Actions › Release build › Run workflow*, choosing the tag
   (or `gh api -X POST repos/25Dark25/obs-lite/actions/workflows/release-build.yml/dispatches -f ref=vX.Y.Z`).
   Download the `obs-lite-<commit>` artifact and unzip it: it contains
   `obs-lite-X.Y.Z-windows-x64.zip` (`obs-lite.exe`, `LICENSE`, `THIRD-PARTY-LICENSES.html`) and
   `SHA256SUMS`. Check that `sha256sum obs-lite-X.Y.Z-windows-x64.zip` matches the hash printed in
   the run's *Package* step and that the run's commit is the tag's (`git rev-parse vX.Y.Z^{commit}`).
   The Windows build is not bit-for-bit reproducible, so a local build will not have the same hash.
   Run `obs-lite.exe --selftest 3 test.mp4` from the zip as a smoke test.

4. **Sign** the zip and check the signature as a user would (*Verifying a release* in the README):

   ```sh
   gpg --local-user A7FCEDEAD7E6C3437312284A442DA687E0FF73EE --detach-sign --armor obs-lite-X.Y.Z-windows-x64.zip
   gpg --verify obs-lite-X.Y.Z-windows-x64.zip.asc obs-lite-X.Y.Z-windows-x64.zip
   ```

5. **Publish**: create a GitHub release from the tag. Use the changelog section as the notes, with
   the zip's SHA-256, and upload `obs-lite-X.Y.Z-windows-x64.zip` and its `.asc`.
