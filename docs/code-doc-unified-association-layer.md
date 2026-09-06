# 代码 ↔ 文档统一关联层（语言无关）实现方案

> 2026-09-06 定稿。目标：把「跨项目代码依赖」「跨路径同主题文档」「代码与文档的概念关联」
> 三条真实世界关系物化到 Memgraph 图中。核心原则：**归属（project/路径）是属性，不是身份**。

## 0. 现状（实测 Memgraph）

- 6 种节点：Project(3) / Document(78) / Entity(3615) / Class(883) / Method(3035) / Module(2)
- 6 种边：CONTAINS / BELONGS_TO / CALLS / MENTIONED_IN / RELATES
- 全部是「单项目内部」边；跨项目边 = 0（RELATES 0、Project 间 0、任意跨 project 0）
- 不同项目提到同一概念 → 各自独立 Entity 节点（同名不同 id），无物理关联
- 代码侧 Class/Method 有 file_path+行号；文档侧 Entity 无代码锚点；两世界间无桥

## 1. 目标模型（增量，不动存量索引）

新增节点：
- `Artifact` — 可被引用的制品单元（jar / crate / wheel / npm 包 / 源码目录）。
  坐标 `(group_id?, artifact_id, version?, type)`；`artifact_id` 全局主键。
  属性：`artifact_id`、`name`、`group_id`、`type`、`language`、`project`（归属，非身份）、
  `path_prefix`（模块根，用于 PART_OF 前缀归属）。
- `Concept`（复用现有 schema 已有 Concept 约束，作为规范概念表根节点）
  属性：`concept_id`、`name`、`aliases`。全库概念身份，跨项目共享。

新增边：
- `(Method|Class) -[:PART_OF]-> (Artifact)` — 符号属于哪个制品（按 file_path 前缀归属）
- `(Artifact) -[:DEPENDS_ON]-> (Artifact)` — 制品间依赖（manifest 坐标解析）
- `(Class|Method) -[:IMPLEMENTS]-> (Entity)` — 代码实现/描述了某文档概念
- `(Document) -[:SAME_TOPIC_AS {similarity}]-> (Document)` — 跨路径同主题
- `(Entity) -[:CANONICAL_OF]-> (Concept)`（或 Entity 挂 canonical_id）— 文档实体收敛到规范概念

## 2. Manifest 解析器（语言无关的分发核心）

输入：项目根。输出：`Vec<ManifestArtifact>`（含依赖坐标）。

| 语言 | 文件 | 解析 |
|------|------|------|
| Java/Maven | `pom.xml` | groupId/artifactId/version + 模块 + dependencies(坐标) |
| Rust | `Cargo.toml` | package + dependencies（path/git/crates.io） |
| Python | `pyproject.toml`/`requirements.txt` | name + dependencies |
| Node | `package.json` | name/version + dependencies |
| Go | `go.mod` | module + require |
| 通用回退 | — | 目录名当 artifact，无依赖 |

坐标 = artifact_id 天然主键，跨项目重复构建幂等（MERGE）。

## 3. 切片（按依赖序推进，每片可独立验证）

- **切片 A：Manifest 解析 + Artifact 落图 + PART_OF**
  代码实体通过 file_path 前缀挂到 Artifact。产出：`dt build` 后每个类知道自己在哪个制品。
- **切片 B：DEPENDS_ON 依赖边（含跨项目）**
  消费端 manifest 坐标 → MERGE Artifact → 与库内已索引 Artifact 对齐建 DEPENDS_ON。
  未索引依赖打 `indexed:false` 占位，对方入库自动补齐（幂等 MERGE）。
- **切片 C：Concept 规范表 + 文档主题归并（SAME_TOPIC_AS）**
  文档身份与路径解耦；Entity 收敛 canonical Concept；同主题跨路径文档建边。
- **切片 D：代码↔文档桥 IMPLEMENTS + dt_search 打通**
  符号名/别名强匹配 + 向量弱匹配；检索命中 Entity 沿桥带代码位置。

## 4. 每条桥接边带证据

- `confidence`：0.0-1.0
- `evidence`：命中字符串 / 坐标 / LLM 理由
- `match_level`：`manifest|exact|alias|vector|llm`

可溯源、可过滤、可审计。

## 5. 里程碑（git tag）

- v0.2.0：切片 A — ✅ 2026-09-06 完成（manifest 解析 + Artifact 落图 + PART_OF，e2e 实证）
- v0.3.0：切片 A+B — ✅ 2026-09-06 完成（DEPENDS_ON 依赖边，含跨项目 jar 依赖，双项目 e2e 实证）
- v0.4.0：切片 A+B+C — ✅ 2026-09-06 文档主题归并完成（`dt merge-docs`：实体重叠系数
  跨路径/跨项目识别同主题文档 → SAME_TOPIC_AS 边；78 篇文档 10 条边实证）
- v0.5.0：切片 A+B+C+D（代码↔文档桥 IMPLEMENTS + dt_search 打通，完整关联层）

## 6. 落点（代码位置）

- `src/domain/types.rs`：ManifestArtifact/ArtifactBlock 等类型
- `src/domain/id.rs`：make_artifact_id
- `src/infrastructure/manifest/`：新目录，语言 manifest 解析器 + registry
- `src/infrastructure/memgraph/schema.rs`：Artifact 约束/索引
- `src/application/build/`：build 编排 + write_graph 加 Artifact 写入 + PART_OF
- `src/interfaces/cli/mod.rs`：`dt build` 无改动（manifest 解析在 build 内自动做）
