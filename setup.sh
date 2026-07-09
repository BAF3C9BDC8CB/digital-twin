#!/bin/bash
# ============================================================
# Digital Twin — 一键部署脚本
# 用法: bash setup.sh
# ============================================================
set -e

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
log()  { echo -e "${GREEN}[✓]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
err()  { echo -e "${RED}[✗]${NC} $1"; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "============================================"
echo " Digital Twin — Setup"
echo "============================================"
echo ""

# ---- 1. 基础依赖 ----
log "检查基础依赖..."
command -v python3  >/dev/null || err "需要 Python 3.10+"
command -v curl     >/dev/null || err "需要 curl"
command -v cargo    >/dev/null || warn "cargo 未安装 (跳过 dt CLI 编译)"

# ---- 2. 外部服务检查 ----
log "检查外部服务..."
for url in "http://localhost:7474" "http://localhost:6333"; do
  if curl -sf "$url" >/dev/null 2>&1; then
    log "  $url 可达"
  else
    warn "  $url 不可达，请确保 Neo4j/Qdrant 已启动"
  fi
done

# ---- 3. 编译 dt CLI ----
log "编译 dt CLI..."
if command -v cargo >/dev/null; then
  cd "$SCRIPT_DIR/engine-rust"
  cargo build --release
  sudo cp target/release/dt /usr/local/bin/dt
  log "dt CLI 已安装: $(which dt)"
else
  warn "跳过 dt 编译，请手动编译 engine-rust/"
fi

# ---- 4. 安装 dt-embed CLI ----
log "安装 dt-embed CLI..."
cd "$SCRIPT_DIR"
pip install -e services/embed-server/ -q 2>/dev/null || \
  pip3 install -e services/embed-server/ -q
sudo ln -sf "$(which dt-embed)" /usr/local/bin/dt-embed 2>/dev/null || true
log "dt-embed CLI 已安装"
log "  验证: dt-embed --info"

# ---- 5. 安装 OpenCode Skill ----
log "安装 OpenCode Skill..."
SKILL_DIR="$HOME/.config/opencode/skills"
mkdir -p "$SKILL_DIR"
if [ -L "$SKILL_DIR/digital-twin" ]; then
  log "Skill symlink 已存在"
elif [ -d "$SKILL_DIR/digital-twin" ]; then
  warn "$SKILL_DIR/digital-twin 是目录而非 symlink，建议手动改为: ln -sf $SCRIPT_DIR $SKILL_DIR/digital-twin"
else
  ln -sf "$SCRIPT_DIR" "$SKILL_DIR/digital-twin"
  log "Skill symlink 已创建: $SKILL_DIR/digital-twin -> $SCRIPT_DIR"
fi

# ---- 6. AGENTS.md 软链 ----
AGENTS_TARGET="$HOME/.config/opencode/AGENTS.md"
mkdir -p "$(dirname "$AGENTS_TARGET")"
if [ -L "$AGENTS_TARGET" ]; then
  log "AGENTS.md symlink 已存在"
else
  [ -f "$AGENTS_TARGET" ] && cp "$AGENTS_TARGET" "$AGENTS_TARGET.bak"
  ln -sf "$SCRIPT_DIR/AGENTS.md" "$AGENTS_TARGET"
  log "AGENTS.md 已软链到 $AGENTS_TARGET"
fi

# ---- 7. 初始化知识图谱 ----
log "初始化 Neo4j Schema..."
if command -v dt >/dev/null 2>&1; then
  dt event --type SchemaInit --entity-id setup --details "schema bootstrap" 2>/dev/null
  log "Schema 将在首次 dt build/index 时通过 ensure_schema() 自动初始化"
else
  warn "dt CLI 未安装，跳过 Schema 初始化（首次 dt build 会自动处理）"
fi

echo ""
echo "============================================"
echo " 部署完成!"
echo "============================================"
echo ""
echo "  1. 验证 dt-embed:"
echo "     dt-embed --info"
echo ""
echo "  2. 索引项目:"
echo "     dt build --path /path/to/project --name my-project"
echo ""
echo "  3. 验证:"
echo "     dt-embed --info                    # 向量化 CLI"
echo "     dt build --path . --name test      # 索引项目"
echo "     dt search \"hello\"                  # 语义搜索"
echo ""
echo "  4. 查看 AGENTS.md 了解 AI 集成规则"
