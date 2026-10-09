#!/bin/bash
# O18 macOS live evidence: runs ON the target Mac. The Windows workstation
# pushes this script plus a credential bundle; results come back as a tar.
# Requires: macOS with SSH reachable, /usr/bin/python3 (ships with macOS).
set -euo pipefail

WORK="$HOME/iyagi-o18"
NODE_VER=v22.14.0
mkdir -p "$WORK"
cd "$WORK"

echo "== $(uname -a)"

# 1) Node 22 (standalone tarball; no brew dependency)
if [ ! -x "$WORK/node/bin/node" ]; then
  echo "== installing Node $NODE_VER"
  curl -fsSLo node.tar.gz "https://nodejs.org/dist/$NODE_VER/node-$NODE_VER-darwin-arm64.tar.gz" \
    || curl -fsSLo node.tar.gz "https://nodejs.org/dist/$NODE_VER/node-$NODE_VER-darwin-x64.tar.gz"
  tar -xzf node.tar.gz
  mv node-$NODE_VER-darwin-* node
fi
export PATH="$WORK/node/bin:$PATH"
node --version

# 2) CLIs
npm install -g @openai/codex @anthropic-ai/claude-code opencode-ai 2>&1 | tail -1
echo "codex: $(codex --version)"
echo "claude: $(claude --version)"
echo "opencode: $(opencode --version)"

# 3) Credentials (bundle pushed from the user's own Windows box, same user)
mkdir -p ~/.codex ~/.claude ~/.local/share/opencode
[ -f bundle/codex-auth.json ] && cp bundle/codex-auth.json ~/.codex/auth.json
[ -f bundle/claude-credentials.json ] && cp bundle/claude-credentials.json ~/.claude/.credentials.json
[ -f bundle/opencode-auth.json ] && cp bundle/opencode-auth.json ~/.local/share/opencode/auth.json
chmod 600 ~/.codex/auth.json ~/.claude/.credentials.json ~/.local/share/opencode/auth.json
codex login status || true

# 4) The repo scripts (pushed alongside)
mkdir -p live
python3 scripts/codex_live_probe.py live/codex_live_transcript_macos.jsonl > live/codex_summary_macos.json 2>&1 || true
grep -o '"outcome": "[a-z]*"' live/codex_summary_macos.json || tail -2 live/codex_summary_macos.json
python3 scripts/claude_live_probe.py live > live/claude_summary_macos.json 2>&1 || true
grep -o '"outcome": "[a-z]*"' live/claude_summary_macos.json || tail -2 live/claude_summary_macos.json
python3 scripts/opencode_live_probe.py live > live/opencode_summary_macos.json 2>&1 || true
grep -o '"outcome": "[a-z]*"' live/opencode_summary_macos.json || tail -2 live/opencode_summary_macos.json

# 5) Package everything for pull-back
tar -czf "$WORK/macos-results.tar.gz" -C "$WORK" live
echo "== results at $WORK/macos-results.tar.gz"
