#!/bin/bash

echo "========== Claude Version =========="
claude --version 2>&1 || echo "claude command not found"

echo
echo "========== Claude Path =========="
which claude

echo
echo "========== Environment Variables =========="
env | grep -E "ANTHROPIC|CLAUDE|API|TOKEN" || echo "No related env vars"

echo
echo "========== Claude Directory =========="
if [ -d "$HOME/.claude" ]; then
    ls -la "$HOME/.claude"
else
    echo "~/.claude does not exist"
fi

echo
echo "========== Claude Config =========="
if [ -f "$HOME/.claude/settings.json" ]; then
    cat "$HOME/.claude/settings.json"
else
    echo "No settings.json"
fi

echo
echo "========== Claude Auth Files =========="
find "$HOME/.claude" -maxdepth 2 -type f 2>/dev/null | sed "s#$HOME#~#"

echo
echo "========== Node Info =========="
node -v
npm -v

echo
echo "========== Process =========="
ps aux | grep -i claude | grep -v grep || echo "No claude process"

echo
echo "========== Done =========="
