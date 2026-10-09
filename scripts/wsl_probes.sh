#!/usr/bin/env bash
# O18 Linux live evidence: run all three probes under a clean Linux PATH
# (Windows interop paths stripped so the Linux CLI builds are selected).
set -e
cd /mnt/d/project/iyagi
export PATH="$HOME/.local/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
export HOME=/home/seen
mkdir -p target/live

echo "=== binaries:"
which codex claude opencode python3

echo "=== codex probe:"
python3 scripts/codex_live_probe.py target/live/codex_live_transcript_linux.jsonl > target/live/codex_summary_linux.json 2>&1 || true
grep -oE '"outcome": "[a-z]+"' target/live/codex_summary_linux.json | head -1 || tail -2 target/live/codex_summary_linux.json

echo "=== claude probe:"
python3 scripts/claude_live_probe.py target/live > target/live/claude_summary_linux.json 2>&1 || true
grep -oE '"outcome": "[a-z]+"' target/live/claude_summary_linux.json | head -1 || tail -2 target/live/claude_summary_linux.json

echo "=== opencode probe:"
python3 scripts/opencode_live_probe.py target/live > target/live/opencode_summary_linux.json 2>&1 || true
grep -oE '"outcome": "[a-z]+"' target/live/opencode_summary_linux.json | head -1 || tail -2 target/live/opencode_summary_linux.json

echo "=== done"
