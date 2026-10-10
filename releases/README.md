# Release binaries

Installers committed for direct download from the README (raw.githubusercontent.com links
served from this repository). Naming: `IYAGI.Term_<version>_<arch>.<ext>`.

What changed in each version is in [RELEASE.md](../RELEASE.md), which also records every
installer's SHA-256; the README download section carries the latest one. Keep this directory
to installers only — one file per platform per version; prune old versions when the history
cost outweighs their value.

## Publishing a release

1. Bump the version in `Cargo.toml` (`[workspace.package]`), `package.json`,
   `package-lock.json` and `src-tauri/tauri.conf.json`, then refresh the lockfile with
   `cargo update -w`.
2. Build on the release Mac: `npm run tauri build`. The DMG lands in
   `target/release/bundle/dmg/IYAGI Term_<version>_<arch>.dmg`.
3. Notarize and staple the DMG (next section), then check that `spctl` reports
   `source=Notarized Developer ID` for the DMG and for the app inside it.
4. Copy the DMG here under the dotted name (`IYAGI.Term_<version>_<arch>.dmg`).
5. Record its SHA-256 (`shasum -a 256`) and size in `README.md`, `README_KR.md` and
   `RELEASE.md` together with the release notes.

## Local macOS signing and notarization

`npm run tauri -- build` prefers the sole valid Developer ID Application certificate
in the login keychain and enables hardened runtime. If several certificates exist,
select one with `APPLE_SIGNING_IDENTITY`. Development builds retain their local
signing behavior. Signing alone does not notarize the app.

On the configured release Mac, notarization credentials are stored in the keychain
profile `iyagi-notary`. Submit the signed app as a ZIP (created with
`ditto -c -k --keepParent`) or submit the signed DMG using
`xcrun notarytool submit <archive> --keychain-profile iyagi-notary --wait`.
After an `Accepted` result, run `xcrun stapler staple <app-or-dmg>`; when distributing
a ZIP, recreate it from the stapled app. To staple the app itself as well, staple it
before packaging it into a DMG. Keep certificates, private keys, and credentials outside
this repository.

Local notarization does not replace the published installers in this directory.
Update the download artifact and its README hash together when publishing a release.

## Trust posture (as of 0.1.1)

macOS installers are signed with a Developer ID, built with hardened runtime and notarized,
with the notarization ticket stapled to the DMG, so they open without Gatekeeper warnings.
They are still built and published from a developer machine rather than CI.

The published SHA-256 travels in this same repository as the DMG it describes. The hash
therefore protects against transport corruption only: anyone able to replace the installer
can replace the hash too. Until the roadmap below lands, downloads from this repository are
exactly as trustworthy as the repository itself.

## Release integrity roadmap

- [x] **Developer ID signing + notarization (macOS)** — done for macOS since 0.1.0; Windows
      and Linux artifacts still need equivalent signing.
- [ ] **CI-built releases on protected refs** — build and publish every installer from a
      protected branch or tag in CI (`.github/workflows/ci.yml`), never from a developer
      machine, so a shipped artifact always corresponds to reviewed code.
- [ ] **Independent hash/signature channel** — publish release hashes or signatures through
      a channel outside this repository (signed release API response or separate trusted
      origin), so the hash and the artifact cannot be swapped together.
