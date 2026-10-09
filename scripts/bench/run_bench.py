#!/usr/bin/env python3
"""Bench driver: build the binaries, then run iyagi-bench.

Replicates scripts/dev-env.sh (GNU toolchain + w64devkit) for hosts without
a complete MSVC install, so `python scripts/bench/run_bench.py --quick`
works from a fresh shell. Stdlib only.

Examples:
  python scripts/bench/run_bench.py --quick            # CI smoke, ~60 s
  python scripts/bench/run_bench.py --release           # full release run
  python scripts/bench/run_bench.py --release --filter latency --filter queue
"""
import argparse
import os
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
W64DEVKIT = Path(os.environ.get("W64DEVKIT_DIR", "D:/tools/w64devkit"))


def build_env() -> dict:
    env = os.environ.copy()
    if (W64DEVKIT / "bin").is_dir():
        env["PATH"] = str(W64DEVKIT / "bin") + os.pathsep + env.get("PATH", "")
    env.setdefault("RUSTUP_TOOLCHAIN", "stable-x86_64-pc-windows-gnu")
    env.setdefault("RUSTFLAGS", "-C link-self-contained=yes")
    return env


def run(cmd: list[str], env: dict) -> int:
    print(f"[bench-driver] {' '.join(str(c) for c in cmd)}", flush=True)
    return subprocess.call(cmd, cwd=REPO, env=env)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--quick", action="store_true", help="reduced sizes, ~60 s CI smoke")
    ap.add_argument("--release", action="store_true", help="build+measure release binaries")
    ap.add_argument("--filter", action="append",
                    choices=["latency", "idle", "flood", "queue"],
                    help="run only these benchmarks (repeatable)")
    ap.add_argument("--no-build", action="store_true", help="skip cargo build (binaries must exist)")
    ap.add_argument("--keep-data", action="store_true", help="keep daemon data dirs")
    args = ap.parse_args()

    env = build_env()
    profile = ["--release"] if args.release else []

    if not args.no_build:
        rc = run(["cargo", "build", "-p", "iyagi-termd", "-p", "term-fixture",
                  "-p", "iyagi-bench", *profile], env)
        if rc != 0:
            return rc

    exe = REPO / "target" / ("release" if args.release else "debug") / (
        "iyagi-bench.exe" if os.name == "nt" else "iyagi-bench")
    if not exe.is_file():
        print(f"[bench-driver] bench binary missing at {exe}", file=sys.stderr)
        return 1
    direct = [str(exe)]
    direct += ["--quick"] if args.quick else []
    direct += ["--release"] if args.release else []
    direct += ["--keep-data"] if args.keep_data else []
    for f in args.filter or []:
        direct += ["--filter", f]
    return run(direct, env)


if __name__ == "__main__":
    sys.exit(main())
