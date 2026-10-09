# Release binaries

Installers committed for direct download from the README (raw.githubusercontent.com links
served from this repository). Naming: `IYAGI.Term_<version>_<arch>.<ext>`.

Each entry's SHA-256 is recorded in the README download section. Keep this directory
to installers only — one file per platform per version; prune old versions when the
history cost outweighs their value.

## Known limitation: current trust posture (as of 0.1.0)

The published SHA-256 travels in this same repository as the DMG it describes, and the
macOS build is ad-hoc signed without notarization. The hash therefore protects against
transport corruption only: anyone able to replace the installer can replace the hash too.
First-launch trust rests on the user's per-version Gatekeeper consent (right-click → Open),
as documented in the README. Until the roadmap below lands, downloads from this repository
are exactly as trustworthy as the repository itself.

## Release integrity roadmap (TODO policy — none of this is implemented yet)

- [ ] **CI-built releases on protected refs** — build and publish every installer from a
      protected branch or tag in CI (`.github/workflows/ci.yml`), never from a developer machine, so a
      shipped artifact always corresponds to reviewed code.
- [ ] **Developer ID signing + notarization** — sign macOS builds with a Developer ID
      certificate and notarize them, replacing ad-hoc signatures (with equivalent signing
      for Windows and Linux artifacts).
- [ ] **Independent hash/signature channel** — publish release hashes or signatures through
      a channel outside this repository (signed release API response or separate trusted
      origin), so the hash and the artifact cannot be swapped together.
