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

3. **Build** the executable: *Actions › Release build › Run workflow*, choosing the tag
   (or `gh api -X POST repos/25Dark25/obs-lite/actions/workflows/release-build.yml/dispatches -f ref=vX.Y.Z`).
   Download the `obs-lite-<commit>` artifact, unzip it, and check that
   `sha256sum obs-lite.exe THIRD-PARTY-LICENSES.html` matches the hashes printed in the run's
   *Checksum* step and that the commit is the tag's
   (`git rev-parse vX.Y.Z^{commit}`). The Windows build is not bit-for-bit reproducible, so a local
   build will not have the same hash.

4. **Source archive**, in the same folder. `.gitattributes` forces LF, so the archive does not
   depend on `core.autocrlf` and is the same for the same commit and git version:

   ```sh
   git archive --format=zip --prefix=obs-lite-X.Y.Z/ -o obs-lite-X.Y.Z-src.zip vX.Y.Z
   ```

5. **Checksums and signatures**:

   ```sh
   sha256sum obs-lite.exe THIRD-PARTY-LICENSES.html obs-lite-X.Y.Z-src.zip > SHA256SUMS
   for f in obs-lite.exe THIRD-PARTY-LICENSES.html obs-lite-X.Y.Z-src.zip SHA256SUMS; do
     gpg --local-user A7FCEDEAD7E6C3437312284A442DA687E0FF73EE --detach-sign --armor "$f"
   done
   ```

   Check them as a user would, following *Verifying a release* in the README.

6. **Publish**: create a GitHub release from the tag, use the changelog section as the notes, and
   upload `obs-lite.exe`, `THIRD-PARTY-LICENSES.html`, `obs-lite-X.Y.Z-src.zip`, `SHA256SUMS`
   and the four `.asc` files.
