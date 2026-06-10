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

# ---- 4. 安装 Embed Server ----
log "部署 Embed Server..."
cd "$SCRIPT_DIR"
if [ ! -d "services/embed-server/venv" ]; then
  python3 -m venv services/embed-server/venv
  source services/embed-server/venv/bin/activate
  pip install -r services/embed-server/requirements.txt -q
  deactivate
fi
log "Embed Server 依赖已安装"
log "  启动: cd services/embed-server && venv/bin/python3 main.py"
log "  验证: curl http://localhost:8001/health"

# ---- 5. 安装 OpenCode Skill ----
log "安装 OpenCode Skill..."
SKILL_DIR="$HOME/.opencode/skills/digital-twin"
mkdir -p "$SKILL_DIR"
if [ -f "SKILL.md" ]; then
  cp "SKILL.md" "$SKILL_DIR/SKILL.md"
  log "Skill 已安装到 $SKILL_DIR"
fi

# ---- 6. AGENTS.md 软链 ----
if [ ! -L "$HOME/AGENTS.md" ]; then
  if [ -f "$HOME/AGENTS.md" ]; then
    cp "$HOME/AGENTS.md" "$HOME/AGENTS.md.bak"
  fi
  ln -sf "$SCRIPT_DIR/AGENTS.md" "$HOME/AGENTS.md"
  log "AGENTS.md 已软链到 $HOME/AGENTS.md"
fi

# ---- 7. 初始化知识图谱 ----
log "初始化 Neo4j Schema..."
python3 -c "
import json, urllib.request
URL = 'http://localhost:7474/db/neo4j/tx/commit'
AUTH = 'Basic bmVvNGo6bmVvNGo='
statements = [
    'CREATE CONSTRAINT IF NOT EXISTS FOR (n:Method) REQUIRE n.method_id IS UNIQUE',
    'CREATE CONSTRAINT IF NOT EXISTS FOR (n:Class) REQUIRE n.class_id IS UNIQUE',
    'CREATE CONSTRAINT IF NOT EXISTS FOR (n:Event) REQUIRE n.event_id IS UNIQUE',
    'CREATE CONSTRAINT IF NOT EXISTS FOR (k:Knowledge) REQUIRE k.id IS UNIQUE',
    'CREATE INDEX IF NOT EXISTS FOR (n:Method) ON (n.project)',
    'CREATE INDEX IF NOT EXISTS FOR (n:Method) ON (n.name)',
    'CREATE INDEX IF NOT EXISTS FOR (n:Event) ON (n.type)',
    'CREATE INDEX IF NOT EXISTS FOR (n:Event) ON (n.timestamp)',
]
for stmt in statements:
    data = json.dumps({'statements': [{'statement': stmt}]}).encode()
    req = urllib.request.Request(URL, data=data,
        headers={'Content-Type': 'application/json', 'Authorization': AUTH})
    urllib.request.urlopen(req, timeout=30)
print('Schema ready')
" 2>/dev/null || warn "Neo4j 不可达，跳过 Schema 初始化"

echo ""
echo "============================================"
echo " 部署完成!"
echo "============================================"
echo ""
echo "  1. 启动 Embed Server:"
echo "     cd services/embed-server && venv/bin/python main.py &"
echo ""
echo "  2. 索引项目:"
echo "     dt build --path /path/to/project --name my-project"
echo ""
echo "  3. 验证:"
echo "     curl http://localhost:8001/health   # Embed Server"
echo "     dt event --type Test --entity-id hello --details 'setup ok'"
echo ""
echo "  4. 查看 AGENTS.md 了解 AI 集成规则"
