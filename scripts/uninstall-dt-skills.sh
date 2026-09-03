#!/bin/bash
# Digital Twin Skill 卸载脚本（统一版本）
# 删除 Hermes skill 目录中的软链接

set -e

echo "=========================================="
echo "Digital Twin Skill 卸载"
echo "=========================================="
echo ""

HERMES_SKILLS="$HOME/.hermes/skills/autonomous-ai-agents"
SKILL_NAME="digital-twin-skill"

echo "🗑️  卸载 Digital Twin Skill..."
echo ""

target_path="$HERMES_SKILLS/$SKILL_NAME"

if [ -L "$target_path" ]; then
    rm "$target_path"
    echo "  ✅ 已删除: $SKILL_NAME"
elif [ -d "$target_path" ]; then
    echo "  ⚠️  $SKILL_NAME (是目录而非软链接，跳过)"
else
    echo "  ℹ️  $SKILL_NAME (不存在)"
fi

echo ""
echo "=========================================="
echo "✅ 卸载完成"
echo "=========================================="
echo ""
