# Releases

Installers are published as **[GitHub Releases](https://github.com/sh2orc/iyagi-term/releases)**,
one release per version, tagged `v<version>` (for example `v0.1.3`). Each release carries its
installers as assets, the release notes for that version and the SHA-256 of every asset.
Installers are no longer committed to this repository; until 0.1.2 they were, and those
DMGs are now attached to their own releases (`v0.1.0` – `v0.1.2`).

What changed in each version is in [RELEASE.md](../RELEASE.md). Asset naming:
`IYAGI.Term_<version>_<arch>.<ext>`.

## Publishing a release

1. **Bump the version** in `Cargo.toml` (`[workspace.package]`), `package.json`,
   `package-lock.json` and `src-tauri/tauri.conf.json` (`npm version <v> --no-git-tag-version`
   covers both npm files), then refresh the lockfile with `cargo update -w`.
2. **Build** on the release Mac: `npm run tauri build`. The DMG lands in
   `target/release/bundle/dmg/IYAGI Term_<version>_<arch>.dmg`.
3. **Notarize and staple** the DMG (next section), then check that `spctl` reports
   `source=Notarized Developer ID` for the DMG and for the app inside it.
4. **Record the hash and notes.** Run `shasum -a 256` on the DMG, then add the release notes
   to `RELEASE.md` and update the download section of `README.md` and `README_KR.md`. Commit
   with the version bump and push to `main`.
5. **Publish** the GitHub Release from that commit, attaching the DMG under its dotted name:

   ```sh
   cp "target/release/bundle/dmg/IYAGI Term_<v>_aarch64.dmg" /tmp/IYAGI.Term_<v>_aarch64.dmg
   gh release create v<v> /tmp/IYAGI.Term_<v>_aarch64.dmg \
     --target main --title "IYAGI Terminal <v>" --notes-file <notes.md> --latest
   ```

   The notes file is the version's section of `RELEASE.md` plus the asset table.
6. **Verify** the published asset: download it from the release page and compare its
   `shasum -a 256` with the recorded hash and with the digest GitHub shows for the asset.

README download buttons point to `releases/latest`, so they follow the newest release; the
direct file links in the tables point to the exact tag.

## Local macOS signing and notarization

`npm run tauri -- build` prefers the sole valid Developer ID Application certificate
in the login keychain and enables hardened runtime. If several certificates exist,
select one with `APPLE_SIGNING_IDENTITY`. Development builds retain their local
signing behavior. Signing alone does not notarize the app.

On the configured release Mac, notarization credentials are stored in the keychain
profile `iyagi-notary`. Submit the signed DMG with
`xcrun notarytool submit <dmg> --keychain-profile iyagi-notary --wait`, or submit the app as a
ZIP (created with `ditto -c -k --keepParent`). After an `Accepted` result, run
`xcrun stapler staple <app-or-dmg>`; when distributing a ZIP, recreate it from the stapled
app. Keep certificates, private keys and credentials outside this repository.

## Trust posture (as of 0.1.4)

macOS installers are signed with a Developer ID, built with hardened runtime and notarized,
with the notarization ticket stapled to the DMG, so they open without Gatekeeper warnings.
They are still built on a developer machine rather than in CI.

The SHA-256 is recorded in `RELEASE.md` and the READMEs, and GitHub computes its own digest
for each release asset. Both still sit under the same GitHub account as the installer, so they
detect transport corruption, not a compromise of that account.

## Release integrity roadmap

- [x] **Developer ID signing + notarization (macOS)** — done for macOS since 0.1.0; Windows
      and Linux artifacts still need equivalent signing.
- [x] **Installers out of the git tree** — since 0.1.3, installers ship as GitHub Release assets
      instead of committed files.
- [ ] **CI-built releases on protected refs** — build and publish every installer from a
      protected tag in CI (`.github/workflows/ci.yml`), never from a developer machine, so a
      shipped artifact always corresponds to reviewed code. Needs a self-hosted macOS runner
      with the signing identity.
- [ ] **Independent hash/signature channel** — publish build provenance or signatures that
      do not depend on the same account (for example artifact attestations from CI), so the
      hash and the artifact cannot be swapped together.
