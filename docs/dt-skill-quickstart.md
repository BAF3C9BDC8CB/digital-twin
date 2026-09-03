# Digital Twin Skill 快速入门

> 5 分钟掌握 Digital Twin 知识图谱操作规范（统一 skill 版）

## 🚀 快速开始

### Step 1: 确认 Skill 已安装

```bash
# 查看 skill 列表（应看到 digital-twin-skill 为 enabled）
hermes skills list | grep digital-twin
```

**注意**：所有 Digital Twin 内容已统一为 **1 个 skill：`digital-twin-skill`**（源文件在项目 `/data/myProject/digital-twin-v2/skills/digital-twin-skill/`，通过软链接接入 Hermes）。

### Step 2: 理解核心概念

#### 代码分析三段序（读码必做）

```
① dt_sense()          → 获取项目全貌（project/indexed/stats/key_entities）
     ↓
② dt_search_kg()      → 定位符号（file_path/start_line/signature/score）
     ↓
③ read_file()         → 验证具体实现
```

**红线**：任何代码文件（.rs/.py/.js/.go/.java 等）的 `read_file` 前，① 和 ② **必须已完成**。

#### 三个世界（World）

- `world=code` → 代码符号（类/方法/函数）— 定位代码用
- `world=doc` → 文档、README — 查架构/规范用
- `world=memory` → 记忆、配置、决策 — 回忆历史用（**统一全局，无需 project**）

---

### Step 3: 实战示例

#### 场景 1: 分析代码（三段序）

```python
# ① 环境感知
sense = dt_sense()
# → project: digital-twin-v2, indexed: true, stats: methods=3028

# ② KG 定位（project 参数取自 dt_sense 结果）
kg = dt_search_kg(
    query="BuildService 构建索引",
    world="code",
    project=sense["project"],
    limit=5
)
# → file_path: src/domain/traits.rs, start_line: 410, score: 0.95
#   只采用 score > 0.7 的结果

# ③ 读码验证（按 KG 返回的区间精确读取）
code = read_file(
    path=kg["hits"][0]["file_path"],
    offset=kg["hits"][0]["start_line"],
    limit=20
)
```

#### 场景 2: 查询配置（记忆优先）

```python
# 先查记忆（配置/凭据/部署历史优先从 memory 检索）
config = dt_search_kg(
    query="Memgraph 连接地址 bolt",
    world="memory",
    limit=5
)
# 命中 → 直接采用；0 命中 → 才读 config.yaml（非 .env）
```

#### 场景 3: 写入记忆（用户说"记住"立即执行）

```python
# 用户："记住，我们决定用 Rust 重写管线"
dt_memorize(
    entity_id="rust-pipeline-rewrite-2026-09",
    details="""name: Rust 管线重写决策
content: 决定用 Rust 重写数据管线，替换现有 Python 实现
summary: 技术选型：Rust 重写管线
type: decision
confidence: high
相关文件: src/pipeline/engine.rs
"""
)
# ⚠️ details 必须是结构化 key: value 格式（必需 name/content，可选 summary/type/confidence/source/tags）
# ⚠️ 工具层 type 参数用 Decision（不是小写 decision），小写用于 details 内 type 字段
```

#### 场景 4: 健康检查（进项目第一步）

```python
health = dt_health()
# 确认 Graph/Vector/Embed 全绿；若项目未索引 → terminal 执行 dt build
```

---

## 📚 Skill 章节速查

`digital-twin-skill` 一个文档包含全部章节，加载一次即可：

| 章节 | 解决什么问题 | 核心工具 |
|------|------------|---------|
| 快速开始 | 健康检查/世界概念/任务路由 | `dt_health`, `dt_sense` |
| 代码分析三段序 | 定位/理解/修改代码 | `dt_sense` → `dt_search_kg(world=code)` → `read_file` |
| 部署与配置管理 | 查配置/凭据/部署历史 | `dt_search_kg(world=memory)` → `read_file(config)` |
| 记忆管理 | 写入/查询记忆 | `dt_memorize` + `dt_search_kg(world=memory)` |
| 健康检查与索引 | 系统状态/索引操作 | `dt_health` + `dt build` |
| 故障排查 | 空结果/服务不可用 | 见各章节排查步骤 |

---

## ⚠️ 常见错误（避免踩坑）

### ❌ 错误 1: 跳过环境感知
```python
# ❌ 不知道 project 名
dt_search_kg(query="BuildService", world="code", limit=5)
# ✅ 先 dt_sense() 拿 project，再带 project 查
```

### ❌ 错误 2: 跳过 KG 定位直接搜文件
```python
# ❌ 盲目 search_files，可能命中测试/注释且无行号
search_files(pattern="BuildService", path="src")
# ✅ KG 定位拿 file_path + start_line，再 read_file 精确区间
```

### ❌ 错误 3: 读取 .env / 输出密钥
```python
# ❌ read_file(path="~/.hermes/.env")
# ❌ "你的 API Key 是 sk-..."
# ✅ 只返回位置提示："密钥在环境变量 OPENAI_API_KEY，配置于 ~/.hermes/.env"
```

### ❌ 错误 4: dt_memorize 用自由文本 details
```python
# ❌ details 必须是结构化 key: value，否则写入内容为空
dt_memorize(entity_id="x", details="一段自由文本说明")
# ✅ details="""name: 标题\ncontent: 正文\n..."""
```

---

## 🎯 五条核心规则

1. **进项目先 `dt_sense()`** - 获取项目全貌
2. **读码前先 `dt_search_kg(world=code)`** - 定位先于读码
3. **查配置先 `dt_search_kg(world=memory)`** - 记忆优先
4. **用户说"记住"立即 `dt_memorize`** - 不要拖延
5. **永远不要读 `.env` 或输出密钥** - 安全第一

---

## 📖 深入学习

```bash
# 查看统一 skill 全文
cat /data/myProject/digital-twin-v2/skills/digital-twin-skill/SKILL.md

# 查看决策层 skill（digital-twin-ops，判断"该不该查"）
cat /data/myProject/digital-twin-v2/skills/digital-twin-ops/SKILL.md
```

**文档索引**（`/data/myProject/digital-twin-v2/docs/`）：
- `digital-twin-skill-system.md` - 架构与设计
- `dt-skill-test-report-v3-unified.md` - 统一 skill 实战测试报告
- `dt-skill-testing-guide.md` - 测试指南

---

## 🧪 验证安装

```bash
/data/myProject/digital-twin-v2/scripts/validate-dt-skills.sh
# 预期：digital-twin-skill 识别 + 软链接正常 + 关键章节完整
```

---

**开始使用吧！**  
从 `skill_view('digital-twin-skill')` 开始你的第一次代码分析 🚀
