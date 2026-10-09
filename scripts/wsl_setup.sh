#!/usr/bin/env bash
# O18 Linux live evidence: install the three CLIs under WSL Ubuntu-24.04.
set -e
export PATH="$HOME/.local/opt/node/bin:$PATH"
echo "== node: $(node --version)"
npm install -g @openai/codex @anthropic-ai/claude-code opencode-ai 2>&1 | tail -3
echo "== codex: $(codex --version)"
echo "== claude: $(claude --version)"
echo "== opencode: $(opencode --version)"
