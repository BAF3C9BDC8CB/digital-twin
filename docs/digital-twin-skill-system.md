# Digital Twin Skill 体系 — 架构指南（v3 统一版）

> **v3 变更（2026-09-03）**：原 5 个独立 skill（code-analysis / deployment / memory / health / knowledge-graph）已**合并为 1 个统一 skill `digital-twin-skill`**。
> 源文件统一放在项目 `skills/` 目录下（Git 管理），Hermes 侧只保留 **1 个软链接**。
> 另保留 `digital-twin-ops`（决策层，v6.0.0，判断"该不该查"）。

## 架构总览

```
/data/myProject/digital-twin-v2/skills/          ← 源文件（Git 管理）
├── digital-twin-skill/SKILL.md                  ← 统一操作指南（424 行）
├── digital-twin-ops/SKILL.md                    ← 决策层（该不该查/预算/降级）
└── README.md

~/.hermes/skills/                                ← Hermes 识别（软链接）
├── autonomous-ai-agents/digital-twin-skill → 项目 skills/digital-twin-skill
└── devops/digital-twin-ops           → 项目 skills/digital-twin-ops
```

**设计原则**：
1. **单一入口**：所有 dt 操作知识收敛到 `digital-twin-skill` 一个文档，1 次 `skill_view()` 即可获得全部指引。
2. **源文件入项目**：skill 属于项目资产，跟随 Git 版本控制，不散落在 `~/.hermes`。
3. **软链接接入**：安装/卸载只是创建/删除软链接，源文件始终在项目内。

## digital-twin-skill 内容结构

统一文档按任务域分章节，通过目录导航：

| # | 章节 | 覆盖内容 |
|---|------|---------|
| 1 | 快速开始 | 健康检查、三个世界（code/doc/memory）、任务路由 |
| 2 | 代码分析三段序 ⭐⭐⭐ | ① dt_sense → ② dt_search(world=code) → ③ read_file；查询技巧；Pitfalls |
| 3 | 部署与配置管理 | 记忆优先检索、禁止读 .env/输出密钥、配置权限表 |
| 4 | 记忆管理 | dt_memorize 结构化 details 格式、三种类型、全局记忆语义 |
| 5 | 健康检查与索引 | dt_health 解读、dt build 全量/增量/单文件 |
| 6 | 故障排查 | 空结果、服务不可用、索引失败 |
| — | 五条核心规则 | 快速参考红线 |

## digital-twin-ops 定位（决策层）

`digital-twin-ops`（v6.0.0）与 `digital-twin-skill` 分工：
- **digital-twin-skill** = 操作层：怎么做（流程/步骤/参数/示例）。
- **digital-twin-ops** = 决策层：该不该查、world/project 语义、token 预算、降级路径（dt-mcp 不可用时走 `dt search`/`dt sense --json` CLI）。

两者互补：操作流程看 skill，判断决策看 ops。

## 任务路由

| 任务类型 | 动作 |
|---------|------|
| 分析/修改代码 | `skill_view('digital-twin-skill')` → 章节 2 三段序 |
| 查配置/凭据 | `skill_view('digital-twin-skill')` → 章节 3 |
| 写入/查询记忆 | `skill_view('digital-twin-skill')` → 章节 4 |
| 检查/索引状态 | `skill_view('digital-twin-skill')` → 章节 5 |
| 不确定该不该查 KG | `skill_view('digital-twin-ops')`（决策层） |

## 安装 / 卸载 / 验证

```bash
# 安装（创建软链接）
/data/myProject/digital-twin-v2/scripts/install-dt-skills.sh

# 验证（识别 + 文件完整性 + 软链接）
/data/myProject/digital-twin-v2/scripts/validate-dt-skills.sh

# 卸载（删除软链接）
/data/myProject/digital-twin-v2/scripts/uninstall-dt-skills.sh
```

## 实战验证结果（2026-09-03）

统一 skill 经子代理实战测试 **3/3 通过**：
- Skill 加载与内容理解（32s / 12 工具调用）
- 代码三段序执行 — 定位 EmbedService trait（37s / 13 工具调用）
- 记忆写入（结构化 details）— 写入 + 召回验证 score 0.86（46s / 21 工具调用）

vs 旧 5-skill 版：时间节省 43%（1m55s vs 3m23s），工具调用减少 37%（46 vs 73）。

详见 `docs/dt-skill-test-report-v3-unified.md`。

## 文件清单

```
skills/
├── digital-twin-skill/SKILL.md        (424 行) 统一操作指南
├── digital-twin-ops/SKILL.md          (决策层)
└── README.md
scripts/
├── install-dt-skills.sh               安装（软链接）
├── uninstall-dt-skills.sh             卸载
└── validate-dt-skills.sh              验证
docs/
├── digital-twin-skill-system.md       本文件（架构）
├── dt-skill-quickstart.md             快速入门
├── dt-skill-test-report-v3-unified.md 统一版实战测试报告
└── dt-skill-testing-guide.md          测试指南
```

> 历史归档（记录 v1/v2 多 skill 时代的探索过程，内容已过时）：
> `dt-skill-delivery.md`、`dt-skill-test-report.md`、`dt-skill-test-report-v2.md`
