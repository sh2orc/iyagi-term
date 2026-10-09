#!/usr/bin/env bash
# O18 Linux live evidence: carry the Windows-side credentials into the WSL
# home (same user, same machine — no credential leaves this host).
set -e
export PATH="$HOME/.local/opt/node/bin:$PATH"
mkdir -p ~/.codex ~/.claude ~/.local/share/opencode

# Codex: auth.json (ChatGPT tokens)
cp /mnt/c/Users/seen/.codex/auth.json ~/.codex/auth.json 2>/dev/null && echo "codex auth copied" || echo "codex auth MISSING"
# Codex config may pin model/provider; copy if present
cp /mnt/c/Users/seen/.codex/config.toml ~/.codex/config.toml 2>/dev/null || true

# Claude: credentials + settings
cp /mnt/c/Users/seen/.claude/.credentials.json ~/.claude/.credentials.json 2>/dev/null && echo "claude credentials copied" || echo "claude credentials MISSING"

# OpenCode: auth.json (Z.AI API key)
cp "/mnt/c/Users/seen/.local/share/opencode/auth.json" ~/.local/share/opencode/auth.json 2>/dev/null && echo "opencode auth copied" || echo "opencode auth MISSING"

echo "== codex login status:"
codex login status 2>&1 | head -2
echo "== opencode auth:"
opencode auth list 2>&1 | tail -6
