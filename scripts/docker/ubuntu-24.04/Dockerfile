# Linux verification image: run the workspace's Linux test suite (PTY
# actors, cgroup paths, process groups, gate helpers) on real Ubuntu 24.04
# from a macOS dev host, instead of cross-compiling. Rust is pinned to the
# workspace MSRV. Build caches live outside the container via the volumes
# shown in the README (Development section).
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential curl ca-certificates pkg-config python3 git procps \
      libssl-dev libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev patchelf \
    && rm -rf /var/lib/apt/lists/*
# Install rustup from a version-pinned installer with a verified hash, instead of
# piping the latest https://sh.rustup.rs script into sh. The image is built for both
# amd64 and arm64 (see README), so select the binary with uname -m. Hashes are the
# official values from
# https://static.rust-lang.org/rustup/archive/<version>/<triple>/rustup-init.sha256 —
# refresh both when bumping RUSTUP_VERSION.
ARG RUSTUP_VERSION=1.28.2
RUN set -eux; \
    case "$(uname -m)" in \
      x86_64) rustup_sha256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c; \
              triple=x86_64-unknown-linux-gnu ;; \
      aarch64) rustup_sha256=e3853c5a252fca15252d07cb23a1bdd9377a8c6f3efa01531109281ae47f841c; \
               triple=aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;; \
    esac; \
    curl -sSfL -o /tmp/rustup-init \
      "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${triple}/rustup-init"; \
    echo "${rustup_sha256}  /tmp/rustup-init" | sha256sum -c -; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --default-toolchain 1.89 --profile minimal -c clippy -c rustfmt; \
    rm -f /tmp/rustup-init
ENV PATH=/root/.cargo/bin:$PATH CARGO_HOME=/cache/cargo-home
WORKDIR /work
