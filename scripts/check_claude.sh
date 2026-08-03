#!/bin/bash

echo "========== Claude 版本 =========="
claude --version 2>&1 || echo "未找到 claude 命令"

echo
echo "========== Claude 路径 =========="
which claude

echo
echo "========== 环境变量 =========="
env | grep -E "ANTHROPIC|CLAUDE|API|TOKEN" || echo "未找到相关环境变量"

echo
echo "========== Claude 目录 =========="
if [ -d "$HOME/.claude" ]; then
    ls -la "$HOME/.claude"
else
    echo "~/.claude 不存在"
fi

echo
echo "========== Claude 配置 =========="
if [ -f "$HOME/.claude/settings.json" ]; then
    cat "$HOME/.claude/settings.json"
else
    echo "未找到 settings.json"
fi

echo
echo "========== Claude 认证文件 =========="
find "$HOME/.claude" -maxdepth 2 -type f 2>/dev/null | sed "s#$HOME#~#"

echo
echo "========== Node 信息 =========="
node -v
npm -v

echo
echo "========== 进程 =========="
ps aux | grep -i claude | grep -v grep || echo "未发现 claude 进程"

echo
echo "========== 完成 =========="
