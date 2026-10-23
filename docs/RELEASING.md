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
   `obs-lite-X.Y.Z-portable-windows-x64.zip` (`obs-lite.exe`, `LICENSE`, `THIRD-PARTY-LICENSES.html`), the
   installer `obs-lite-X.Y.Z-windows-x64.msi` (the same files), the Linux bundle
   `obs-lite-X.Y.Z-linux-x86_64.flatpak` (with `LICENSE` and the Linux `THIRD-PARTY-LICENSES.html`
   in `/app/share/licenses`) and `SHA256SUMS`. Check that
   `sha256sum --check SHA256SUMS` passes, that the hashes match those printed in the run's *Package*
   step and that the run's commit is the tag's (`git rev-parse vX.Y.Z^{commit}`).
   The Windows build is not bit-for-bit reproducible, so a local build will not have the same hash.
   Run `obs-lite.exe --selftest 3 test.mp4` from the zip as a smoke test, and install the `.msi`:
   *Obs Lite* must be in the Start menu and start; then uninstall it. On Linux, install the bundle
   with `flatpak install --user obs-lite-X.Y.Z-linux-x86_64.flatpak`: *Obs Lite* must be in the
   applications menu, start and record; then `flatpak uninstall --user io.github._25Dark25.ObsLite`.

4. **Sign** the zip, the installer and the Flatpak bundle and check the signatures as a user would
   (*Verifying a release* in the README):

   ```sh
   for f in obs-lite-X.Y.Z-portable-windows-x64.zip obs-lite-X.Y.Z-windows-x64.msi \
            obs-lite-X.Y.Z-linux-x86_64.flatpak; do
     gpg --local-user A7FCEDEAD7E6C3437312284A442DA687E0FF73EE --detach-sign --armor "$f"
     gpg --verify "$f.asc" "$f"
   done
   ```

5. **Publish**: create a GitHub release from the tag. Use the changelog section as the notes, with
   the SHA-256 of the zip, the installer and the bundle, and upload the three with their `.asc`
   files. Add a `<release>` for the version to
   `packaging/flatpak/io.github._25Dark25.ObsLite.metainfo.xml` in the preparation step.
