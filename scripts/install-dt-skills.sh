#!/bin/bash
# Digital Twin Skill 安装脚本（统一版本）
# 将项目中的 digital-twin-skill 软链接到 Hermes skill 目录

set -e

echo "=========================================="
echo "Digital Twin Skill 安装"
echo "=========================================="
echo ""

# 定义路径
PROJECT_ROOT="/data/myProject/digital-twin-v2"
HERMES_SKILLS="$HOME/.hermes/skills/autonomous-ai-agents"
SKILL_NAME="digital-twin-skill"

# 检查项目路径
if [ ! -d "$PROJECT_ROOT/skills/$SKILL_NAME" ]; then
    echo "❌ 错误：找不到项目 skill 目录：$PROJECT_ROOT/skills/$SKILL_NAME"
    exit 1
fi

# 检查 Hermes skill 目录
if [ ! -d "$HERMES_SKILLS" ]; then
    echo "⚠️  创建 Hermes skill 目录：$HERMES_SKILLS"
    mkdir -p "$HERMES_SKILLS"
fi

echo "📦 安装 Digital Twin Skill..."
echo ""

source_path="$PROJECT_ROOT/skills/$SKILL_NAME"
target_path="$HERMES_SKILLS/$SKILL_NAME"

if [ ! -d "$source_path" ]; then
    echo "  ❌ $SKILL_NAME (源目录不存在)"
    exit 1
fi

# 删除旧的软链接或目录
if [ -L "$target_path" ]; then
    rm "$target_path"
elif [ -d "$target_path" ]; then
    echo "  ⚠️  $SKILL_NAME (目标位置已存在目录，跳过)"
    exit 1
fi

# 创建软链接
ln -sf "$source_path" "$target_path"

if [ -L "$target_path" ]; then
    echo "  ✅ $SKILL_NAME → $source_path"
else
    echo "  ❌ $SKILL_NAME (软链接创建失败)"
    exit 1
fi

echo ""
echo "=========================================="
echo "✅ 安装完成"
echo "=========================================="
echo ""

# 验证安装
if [ -L "$HERMES_SKILLS/$SKILL_NAME" ]; then
    echo "✅ Digital Twin Skill 安装成功！"
    echo ""
    echo "下一步："
    echo "  1. 查看 skill 列表: hermes skills list | grep digital-twin"
    echo "  2. 使用 skill: hermes chat"
    echo "  3. 加载 skill: skill_view('digital-twin-skill')"
    echo ""
else
    echo "⚠️  安装失败，请检查错误信息"
    exit 1
fi
