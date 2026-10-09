#!/bin/bash
# Runs on the WINDOWS workstation (Git Bash). Polls the Mac mini until it
# wakes, then pushes the macOS probe payload and pulls results back.
# Usage: bash scripts/macos_remote_probes.sh [mac-ip] [max-wait-seconds]
set -uo pipefail
cd "$(dirname "$0")/.."

MAC="${1:-100.101.10.110}"
MAX_WAIT="${2:-600}"
START=$(date +%s)

echo "polling $MAC (tailscale ping) for up to ${MAX_WAIT}s..."
while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START))
  if [ "$ELAPSED" -ge "$MAX_WAIT" ]; then
    echo "MAC_WAKE_TIMEOUT after ${ELAPSED}s"
    exit 3
  fi
  if tailscale ping --timeout 5s --c 1 "$MAC" 2>&1 | grep -qE "pong|via"; then
    echo "MAC_AWAKE after ${ELAPSED}s"
    break
  fi
  sleep 15
done

# Wait for SSH to come up (Tailscale path first, then port 22).
for i in $(seq 1 40); do
  if ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new "$MAC" true 2>/dev/null; then
    echo "SSH_READY"
    break
  fi
  sleep 5
  if [ "$i" = 40 ]; then echo "SSH_NEVER_READY"; exit 4; fi
done

# Push payload.
ssh "$MAC" "mkdir -p ~/iyagi-o18/bundle ~/iyagi-o18/scripts"
scp target/mac-bundle/* "$MAC:iyagi-o18/bundle/"
scp scripts/codex_live_probe.py scripts/claude_live_probe.py scripts/opencode_live_probe.py "$MAC:iyagi-o18/scripts/"
scp scripts/macos_probes.sh "$MAC:iyagi-o18/"
ssh "$MAC" "bash ~/iyagi-o18/macos_probes.sh"
scp "$MAC:iyagi-o18/macos-results.tar.gz" target/macos-results.tar.gz
echo "RESULTS_PULLED target/macos-results.tar.gz"
