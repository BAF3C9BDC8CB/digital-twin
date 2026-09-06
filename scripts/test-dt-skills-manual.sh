#!/bin/bash
# Digital Twin Skill 实战测试脚本
# 使用子代理测试 skill 的实际使用效果

set -e

PROJECT_ROOT="/data/myProject/digital-twin-v2"
TEST_LOG="/tmp/dt-skill-test-$(date +%s).log"

echo "=========================================="
echo "Digital Twin Skill 实战测试"
echo "=========================================="
echo ""
echo "测试日志: $TEST_LOG"
echo ""

# 测试 1: 加载代码分析 skill
echo "测试 1: 加载代码分析 skill"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

hermes chat -q "请加载 digital-twin-code-analysis skill 并告诉我代码分析的三段序是什么？" 2>&1 | tee -a "$TEST_LOG"

echo ""
echo "✓ 测试 1 完成"
echo ""

# 测试 2: 实际使用三段序分析代码
echo "测试 2: 使用三段序分析 BuildService"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

hermes chat -q "进入项目 /data/myProject/digital-twin-v2，使用代码分析三段序找到 BuildService 类的实现。请严格遵循：① dt_sense() ② dt_search(world=code) ③ read_file()" 2>&1 | tee -a "$TEST_LOG"

echo ""
echo "✓ 测试 2 完成"
echo ""

# 测试 3: 查询配置
echo "测试 3: 使用部署 skill 查询配置"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

hermes chat -q "加载 digital-twin-deployment skill，然后查询 Memgraph 的连接配置。记住：优先从 world=memory 检索，不要读取 .env 文件。" 2>&1 | tee -a "$TEST_LOG"

echo ""
echo "✓ 测试 3 完成"
echo ""

echo "=========================================="
echo "所有测试完成"
echo "=========================================="
echo ""
echo "测试日志已保存到: $TEST_LOG"
echo ""
echo "请检查日志，验证："
echo "  1. Skill 是否成功加载"
echo "  2. 三段序是否被正确执行"
echo "  3. 是否遵循了安全规则（不读取 .env）"
