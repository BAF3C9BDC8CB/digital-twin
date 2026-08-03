# dt sense —— 一条命令完成环境感知（设计）

**日期**：2026-08-03
**状态**：已定稿（用户评审通过，指令式触发）
**前置**：命令面清理（CLI 27→16、MCP 33→24，查询入口统一为 `dt search`）
**触发**：AGENTS.md 的"最小环境感知→提取关键词→查 KG"流程依赖 AI 多步分析，步骤多、易走样；用户要求感知逻辑固化进 `dt` 命令——AI 编辑器打开项目后执行一条命令即得环境简报，**不依赖编辑器的 AI 进行分析**；当前目录已存向量时返回目录大概内容。

---

## 1. 问题陈述

| 现状 | 问题 |
|------|------|
| AGENTS.md 规定 AI 会话开始要"读根目录→提取关键词→查 KG→深入探索" | 4 步全靠 AI 自觉执行，走样率高；读目录/猜关键词浪费上下文 |
| PROJECT-DISCOVERY.md 的项目发现流程 | 同样是 AI 手工流程（读 config.yaml、扫子目录、比对去重） |
| 项目是否已索引、索引了什么内容 | 无单一命令可查，AI 只能试探性搜索 |

后果：每次会话开始消耗大量 token 做确定性本可完成的感知；感知质量随 AI 状态波动。

## 2. 目标与非目标

**目标**：
- 新命令 `dt sense [path]`：定位 → 注册匹配 → 索引状态 → 输出**项目简报**或**发现报告**，全程确定性、零 AI 分析
- 已索引目录返回"目录大概内容"：统计 + 目录画像 + 语言分布 + 关键实体
- `--json` 结构化输出；MCP 第 25 个工具 `dt_sense` 透传
- AGENTS.md/SKILL.md 流程改写：会话第一个动作 = `dt_sense`

**非目标**：
- 不做项目级 LLM 自然语言摘要（P2，需 build 时写入 `Project.summary`）
- 不做 KG 关联上下文段（该项目的基础设施/部署事件，P2）
- 不做 opencode 插件自动注入（P3，已确认指令式先行）
- 不改动任何写入/索引路径（sense 是只读命令）

## 3. 目标架构

```
┌─ CLI: dt sense [path] [--json]        （人类简报 / JSON）
└─ MCP: dt_sense(path?)                 （subprocess 调 CLI --json 透传）
        │  全部委托
        ▼
┌─────────────────────────────────────────────┐
│  SenseService（src/application/sense/）       │
│  1. locate(path)   → git root + config 匹配  │
│  2. status(project)→ 三态判定                │
│  3. brief(project) → 简报（已索引）           │
│  4. discover(dir)  → 发现报告（未注册）        │
└─────────────────────────────────────────────┘
        │ 只读
        ▼
  Qdrant code_methods（payload）/ Memgraph（Method/Class/CALLS）/ SQLite（snapshots）
```

## 4. 设计决策

### S-D1：数据源分工（按各库所长）

| 数据 | 来源 | 理由 |
|------|------|------|
| 向量数、目录画像、语言分布 | Qdrant `code_methods` scroll（payload only，不取向量） | payload 含 `project`/`file_path`，图缺失时仍可用 |
| 方法数、类数、关键实体 | Memgraph `Method`/`Class`/`CALLS` | 图计数 + CALLS 入度 |
| 最近构建时间 | SQLite snapshots 表 `MAX(updated_at)`（按项目前缀过滤） | build 增量缓存天然记录时间 |

Qdrant scroll 分页 256/页、仅 payload；万级方法项目约 40 次请求，一次性命令可接受。

### S-D2：三态状态机

```
locate(path) 匹配 config.yaml projects（base+name 拼全路径，最长前缀匹配）
  ├─ 命中且 Qdrant vectors > 0  → indexed             → 输出简报
  ├─ 命中但 vectors = 0          → registered_not_indexed → 简报仅基本信息 + 建议 dt build --name X
  └─ 未命中                      → unregistered        → 输出发现报告
```

config.yaml 缺失/无 projects 段 → 视为 unregistered。

### S-D3：关键实体双降级

1. 首选：CALLS 入度 top 10（`MATCH (m:Method {project:$p})<-[:CALLS]-() RETURN m.name, m.file_path, count(*) AS d ORDER BY d DESC LIMIT 10`）
2. 图数据不足（0 行）：名称启发式——`Class` 名匹配 `*Controller|*Service|*Mapper|*Application` 或 `main` 函数，取 10 个
3. 仍为空：省略该段

### S-D4：发现报告（unregistered 分支）

- 只扫输入 path 的**一级子目录**（性能；嵌套项目由用户在子目录再跑 sense 覆盖）
- 候选判定：目录内 depth ≤ 2 存在 ≥1 个源码文件（扩展名集合复用 `scanner` 配置）
- 排除：`scanner.ignore_dirs` + `~/.config/digital-twin/ignored_dirs.yaml`
- 每个候选输出建议命令：`dt build --path <dir> --name <dirname>`

### S-D5：JSON 契约

```json
{
  "status": "indexed | registered_not_indexed | unregistered",
  "project": { "name": "...", "path": "...", "registered": true },
  "stats":    { "methods": 0, "classes": 0, "vectors": 0, "last_build": "ISO8601|null" },
  "dirs":     [ { "dir": "src/controller", "methods": 32, "classes": 5 } ],
  "languages":[ { "ext": ".java", "pct": 80 } ],
  "key_entities": [ { "name": "...", "kind": "method|class", "source": "in_degree|heuristic", "in_degree": 12 } ],
  "candidates": [ { "path": "...", "suggested_name": "...", "build_cmd": "dt build --path ... --name ..." } ],
  "degraded": [ "qdrant | memgraph | sqlite" ]
}
```

- `dirs`/`languages` 由 Qdrant payload 聚合：file_path 相对项目根取**第一段目录**聚合（根级文件归入 `"."`）
- 人类可读格式为同一数据的渲染，字段一一对应
- 后端不可用 → 对应字段空 + `degraded` 标注，**整体不报错**（sense 必须永远成功退出 0）

### S-D6：MCP `dt_sense`（第 25 个工具）

- `dt_sense(path?: string)` → subprocess `dt sense [path] --json`，stdout JSON 透传
- 参数校验：path 缺省时 CLI 侧取 cwd（MCP 工作目录即用户项目目录）

### S-D7：命名

`dt sense`（候选 here/overview 被否：sense 准确表达"环境感知"语义）。

## 5. 文档更新（P1 组成部分）

- **AGENTS.md**：执行顺序改写为 `1. dt_sense（第一个动作）→ 2. 按场景 dt_search/dt_search_kg → 3. 深入探索`；旧"读目录+提取关键词"流程删除；场景 A/B/C 查询策略保留
- **SKILL.md**：核心流程表加"会话开始 → `dt_sense`"
- **PROJECT-DISCOVERY.md**：降级为"sense 未注册分支的规则参考"（规则已固化进代码）
- **README.md / CLAUDE.md**：CLI 16→17、MCP 24→25，命令表加 `dt sense`

## 6. 错误处理

| 场景 | 行为 |
|------|------|
| config.yaml 不存在 | 视为 unregistered，走发现报告 |
| Qdrant 不可用 | vectors=0 不成立——无法判定索引：status 按 Memgraph 方法数兜底判定（>0 → indexed），degraded += qdrant；dirs/languages 段省略 |
| Memgraph 不可用 | stats/key_entities 省略，degraded += memgraph；status 按 Qdrant 判定 |
| 双库不可用 | status=unregistered 时仍可输出发现报告（纯文件系统）；否则报 degraded 简报 |
| path 不存在 | 报错退出非 0（唯一失败场景） |

## 7. 测试

- **单测**：
  - 前缀匹配：临时 config.yaml fixture（多项目、嵌套路径、rel_path 形式、最长前缀胜出）
  - 目录画像聚合：fixture payload 列表 → dirs/languages 统计正确性
  - 候选扫描：临时目录树（含 node_modules 排除、嵌套源码、ignored_dirs.yaml）
  - 状态机三态分支（stub 三后端）
- **集成（live，`#[ignore]`）**：对 `test/` fixture 项目 build 后 `dt sense --json` 断言 status/stats/dirs 字段
- **基线**：675 passed / 2 预存失败不增加；clippy 0 error

## 8. 阶段切分

- **P1（本 spec）**：命令本体 + `--json` + MCP `dt_sense` + 文档流程更新
- **P2**：KG 关联上下文段（infra/事件）+ build 时项目级 LLM 摘要（`Project.summary`）
- **P3**：opencode 插件自动注入（顺带修复 `~/.config/opencode/plugins/dt-build.js` 断链）
