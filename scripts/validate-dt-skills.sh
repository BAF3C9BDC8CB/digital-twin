#!/bin/bash
# Digital Twin Skill 验证脚本（统一版本）
# 验证 skill 是否被正确识别和加载

set -e

echo "=========================================="
echo "Digital Twin Skill 验证"
echo "=========================================="
echo ""

# 检查 skill 是否被 Hermes 识别
echo "✓ 检查 skill 识别状态..."
hermes skills list | grep "digital-twin-skill" > /tmp/dt-skill.txt

echo ""
echo "已识别的 Digital Twin Skill:"
echo "----------------------------"
cat /tmp/dt-skill.txt
echo ""

if grep -q "digital-twin-skill" /tmp/dt-skill.txt; then
    echo "  ✅ digital-twin-skill"
else
    echo "  ❌ digital-twin-skill (未找到)"
    exit 1
fi

echo ""
echo "✓ 所有 skill 已被识别"
echo ""

# 检查 skill 文件完整性
echo "✓ 检查 skill 文件完整性..."
skill_dir="$HOME/.hermes/skills/autonomous-ai-agents"
skill_path="$skill_dir/digital-twin-skill/SKILL.md"

if [ -f "$skill_path" ]; then
    lines=$(wc -l < "$skill_path")
    echo "  ✅ digital-twin-skill ($lines 行)"
else
    echo "  ❌ digital-twin-skill (文件不存在)"
    exit 1
fi

echo ""
echo "✓ Skill 文件完整"
echo ""

# 检查 skill 关键内容
echo "✓ 检查 skill 关键内容..."

if grep -q "代码分析三段序" "$skill_path" && \
   grep -q "部署与配置管理" "$skill_path" && \
   grep -q "记忆管理" "$skill_path" && \
   grep -q "健康检查与索引" "$skill_path"; then
    echo "  ✅ digital-twin-skill 包含所有核心章节"
else
    echo "  ❌ digital-twin-skill 缺少核心章节"
    exit 1
fi

echo ""
echo "✓ Skill 关键内容完整"
echo ""

# 检查软链接
echo "✓ 检查软链接..."
link_path="$skill_dir/digital-twin-skill"

if [ -L "$link_path" ]; then
    target=$(readlink -f "$link_path")
    echo "  ✅ digital-twin-skill → $target"
else
    echo "  ❌ digital-twin-skill (不是软链接)"
    exit 1
fi

echo ""
echo "✓ 软链接正常"
echo ""

# 统计信息
echo "=========================================="
echo "统计信息"
echo "=========================================="
echo ""

total_lines=$(wc -l < "$skill_path")
echo "总行数: $total_lines"
echo ""

echo "=========================================="
echo "✅ 验证通过！Digital Twin Skill 完整可用"
echo "=========================================="
echo ""
echo "下一步："
echo "  1. 使用 skill: hermes chat -q 'skill_view(\"digital-twin-skill\")'"
echo "  2. 查看文档: cat $skill_path"
echo ""
