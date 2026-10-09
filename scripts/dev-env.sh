#!/usr/bin/env bash
# Development environment for hosts where the MSVC toolchain is incomplete
# (e.g. missing Windows SDK libs). Sources nothing; print usage:
#
#   source scripts/dev-env.sh
#
# Sets the GNU toolchain + portable w64devkit (D:/tools/w64devkit) and the
# self-contained link flags the rust-mingw component needs. CI runners with a
# complete MSVC install do NOT source this file.

export W64DEVKIT_DIR="${W64DEVKIT_DIR:-/d/tools/w64devkit}"
if [ -d "$W64DEVKIT_DIR/bin" ]; then
  case ":$PATH:" in
    *":$W64DEVKIT_DIR/bin:"*) ;;
    *) export PATH="$W64DEVKIT_DIR/bin:$PATH" ;;
  esac
else
  echo "dev-env: w64devkit not found at $W64DEVKIT_DIR (GNU builds will fail)" >&2
fi

export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable-x86_64-pc-windows-gnu}"
export RUSTFLAGS="${RUSTFLAGS:--C link-self-contained=yes}"
echo "dev-env: toolchain=$RUSTUP_TOOLCHAIN rustflags=$RUSTFLAGS"
