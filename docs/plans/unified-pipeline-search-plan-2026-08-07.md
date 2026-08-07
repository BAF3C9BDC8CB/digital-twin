# 统一核心 Pipeline —— 搜索输出与 Nacos 配置接入收敛方案

> 状态：实施计划（用户已批准组建团队执行，worker 模型 deepseek-v4-flash，审核 kimi-k3 本会话）。
> 日期：2026-08-07。
>
> 取代 `nacos-llm-first-implementation-plan.md` 中"独立 LLM-first 管线"的定位——
> Nacos 不建独立处理链，全部特殊性收敛为**前置适配**，核心链路与普通文件完全一致。

## 0. 目标形态（用户确认）

搜索结果统一渲染，配置类命中示例：

```
[0.0320] [nacos配置/ConfigKey] spring.cloud.nacos.discovery
  分析: 配置应用连接 Nacos 注册中心并启用服务发现。
  来源: dt://nacos/test/DEFAULT_GROUP/common.yaml#spring.cloud.nacos
  正文:                       ← 仅 --show-content 或精确命中时显示
    spring:
      cloud:
        nacos:
          discovery:
            server-addr: nacos-headless.nacos-test.svc.cluster.local:8848
            namespace: af6d04ec-7142-47af-89bf-0e6009f40bc1  #命名空间 代指某个环境
```

四条设计决策（用户已确认方向，细节见 §4 决策点）：

1. **文件类型标签**：Nacos 来源命中显示 `[nacos配置/ConfigKey]`——`file_type` 维度新增 `nacos_config` 类别（显示名 `nacos配置`），与扩展名体系正交（按 `dt://nacos/` 前缀或 payload `source_type=nacos` 判定）。
2. **分析字段统一走 LLM**：配置 chunk 与代码方法共用同一条 LLM 分析节点契约，写入 `llm_analysis`；`search_config.rs` 的硬编码 `config_purpose_summary()` 删除。
3. **正文字段通用化**：`SearchHit.content` 本就通用（Method/Doc/Config 都填）。默认渲染**一律不显示正文**（紧凑三行制），新增 `--show-content` 显式展开；代码方法展开时显示 `file_path:Ls-Le` 原文片段，配置展开时显示 section 原文——同一字段同一渲染分支，无特例。
4. **来源格式**：`dt://nacos/{namespace_name}/{group}/{dataId}#{key路径}`。删除当前 environment 段（实测 payload `environment` 为空、代码兜底假数据 "test"）。锚点用 key 路径直接写，不用 `section=` 前缀。

统一后的核心链路：

```
任意来源(本地文件 / Nacos / Jenkins)
  → VirtualFile(virtual_path, content, project)         ← Phase 0 已有
  → Chunk(AST / 文档段 / 配置段,同一 Processor 接口)
  → LLM 分析(同一契约,输出 llm_analysis)
  → Store(Qdrant payload: content + llm_analysis + source_ref + file_type + entity_type)
  → 统一渲染([file_type_label/entity_type] title + 分析 + 来源,content 按需展开)
```

## 1. 前置状态（开工前必读）

### 1.1 未提交改动归位（T0）

当前工作区 7 个修改文件 + 1 个未跟踪目录（均为本次相关领域的既有工作）：

```
M src/application/context/fusion.rs
M src/application/context/search_config.rs
M src/application/context/search_mcp.rs
M src/application/context/search_memory.rs
M src/application/knowledge/extract/retrieve.rs
M src/interfaces/cli/search_render.rs
M src/interfaces/grpc/services/build_service.rs
?? docs/plans/
```

T0 任务：`cargo check` 确认可编译 → 提交为单独 commit（message: `chore: 归位统一架构前置改动`），作为团队工作的基线。**不允许 worker 丢弃这些改动。**

### 1.2 与 Phase 0 G1-G6 的关系

本方案**只做搜索侧收敛 + Nacos 前置补强**，不修复 G1-G6 全部项，但有两处交集：
- G3（`select_prompt` 无 nacos_config 分支）：本方案 T3 会为配置 chunk 增加 LLM 分析，届时**顺便**接 nacos 路由（document_with_nlp 继续兜底也行，先保证 llm_analysis 有真实输出）。
- G4（EntityType 枚举与配置词表）：本方案新增 `ConfigKey`/`ConfigSection` 作为 entity_type 使用时，枚举已有 `Config`；如需新增 `ConfigKey` 变体，按"枚举 + as_str + Deserialize 三处同改"的铁律执行（见 digital-twin-ops 技能）。

G1（CLI `dt build --source nacos` 接通）、G2、G5、G6 **不在本方案范围**——但 T3 的组件层验证会用 harness 直接喂 Nacos VirtualFile，绕开 G1。

### 1.3 环境现实

- qwen3.5 仅 CPU 模式可用（GPU OOM/xllamacpp 回归），~30-60s/查询。**T3 的 LLM 分析验证用 1-3 条配置样本即可，不做全量回填**。
- Memgraph `bolt://localhost:7688`、Qdrant `:6333` 在线；config_chunks 现有 1607 点（无 resource_type，text 保留原始缩进——已验证）。
- deepseek-v4-flash 凭据已探测可用（chat/completions 正常返回）。

## 2. 任务分解（看板 team 板，全部 --model deepseek-v4-flash）

### T0 基线归位（无依赖，最先）
- `cargo check` 绿 → 提交 7 个修改文件 + `docs/plans/`（本计划文档随此 commit 入库）。
- 产出：基线 commit SHA。

### T1 渲染与类型维度收敛（依赖 T0）
范围：`src/domain/file_type.rs`、`src/application/context/search_mcp.rs`、`src/interfaces/cli/search_render.rs`

1. `FileCategory` 新增 `NacosConfig` 变体：slug `nacos_config`，label `nacos配置`。
2. `infer_file_type_pub`：路径以 `dt://nacos/` 开头 → 直接返回 `NacosConfig`（不走后缀映射）。注意 `source_ref` 推断路径在 postprocess_hits 已有 `file_path → source_ref → id` 回退链，确保 dt:// 前缀分支在链上生效。
3. `resolve_file_types`：接受 `nacos_config` / `nacos` / `nacos配置` → `vec![NacosConfig]`；`all` 集合加入该变体。
4. 渲染层 `--show-content`：
   - `dt search` CLI 新增 `--show-content` 标志（clap 参数，透传 SearchRequest 或渲染参数——选改动面小的方式，倾向渲染参数，不动 proto）。
   - `render_hit` 改为：`content` 存在且（`--show-content` 开启）→ 输出 `  正文:` 块（保留现有 4 空格缩进、逐行原文）；默认不输出。删除 ConfigChunk/ConfigKey 的无条件正文分支。
   - Method 展开时同样输出 `content`（代码片段原文）；Doc 同理。
5. 单元测试：`file_type.rs`（新类别映射/解析）、`search_render.rs`（默认无正文、--show-content 有正文、ConfigKey/Method/Doc 三形态）。

**验收**：`cargo test --release file_type search_render` 全绿；`dt search "xxx" --world config` 默认无正文，`--show-content` 有正文且逐字符与 payload text 一致。

### T2 来源格式修正（依赖 T0，与 T1 并行）
范围：`src/application/context/search_config.rs`（两处 source_ref 构造：~L240、~L296）

1. 格式改为 `dt://nacos/{namespace}/{group}/{data_id}#{section}`：
   - 删除 environment 段（及 `unwrap_or("test")` 假数据兜底）。
   - namespace 为空时兜底 `public`，group 兜底 `DEFAULT_GROUP`（保留现有逻辑）。
   - 锚点 `#section=` → `#{section_name}`。
2. L296 的硬编码 `dt://nacos/test/public/DEFAULT_GROUP/config#section=...` 同步修正。
3. 检查 `src/application/sync/nacos/`（kg_bridge 等）是否有其他 source_ref 构造点，一并统一。
4. 单元测试：构造含/不含 namespace 的 payload，断言来源串精确格式。

**验收**：`dt search "server-addr" --world config` 输出来源为 `dt://nacos/test/DEFAULT_GROUP/uvp-common.yaml#spring.cloud` 形态，无 environment 段。

### T3 配置 LLM 分析 + 硬编码摘要删除（依赖 T1）
范围：`src/application/pipeline/processors/`、`src/application/context/search_config.rs`

1. 配置 chunk 的 LLM 分析：走统一 LLM 分析节点（与代码方法同一 `llm_analysis` 契约）。组件层 harness：构造 Nacos VirtualFile（yaml 样本 3 条：datasource / redis / nacos discovery），跑 Chunk→LLM(qwen3.5 CPU)，断言产出 `llm_analysis` 且写入 Qdrant payload。
2. `search_config.rs`：删除 `config_purpose_summary()` 及两个调用点，`llm_analysis` 读 payload 字段，空则 `None`（渲染层已有"暂无摘要"回退）。
3. select_prompt 的 nacos 路由：配置 chunk 用 `document_with_nlp` 或专用配置 prompt（worker 评估改动面，优先复用现有模板，**不新增 EntityType 变体**，避免 G4 枚举同步问题在本任务内爆炸）。若 LLM 输出类型词表外，按现有归一化落 `Other` 即可——本任务不追求实体类型完美。
4. 单测：mock LLM 返回，断言 SearchHit.llm_analysis 透传；删除 config_purpose_summary 后无残留引用。

**验收**：harness 跑 3 条样本有真实 llm_analysis；`grep -rn config_purpose_summary src/` 为空；`cargo test --release` 不新增失败。

**纪律**：LLM 验证限 3 条样本（CPU ~40s/条），禁止全量回填 config_chunks。

### T4 回归与验收（依赖 T1+T2+T3）
1. `cargo fmt --check`（既有差异隔离说明）、`cargo test --release`：694 基线，0 新增失败。
2. golden set `run_regression.py` 12/12。
3. 端到端：`dt search` 实际查询验证目标形态（nacos discovery、datasource、redis 各一），截图/输出存档。
4. MCP 路径：`dt_search --world config` JSON 纯净（stderr 不混入——已知 run_cmd 合并问题，若出现只记录不修复，属另一任务）。
5. 产出验收报告 `/data/doc/unified-pipeline-search-acceptance.md`。

### 依赖图

```
T0 ──→ T1 ──→ T3 ──→ T4
 └────→ T2 ──────────┘
```

## 3. Worker 纪律（写进每个任务 body）

- 产出即结束，不反复修改；每个任务先 `cargo check` 再动手，完成后 `cargo test --release <相关模块>`。
- 只动任务范围内文件；发现范围外问题写进完成报告，不擅自修。
- 不 `git commit` 之外的 git 写操作（不 push、不 reset）；commit message 带任务 ID。
- 遇到 qwen3.5 不可用：T3 用 mock 验证代码路径，LLM 实测留给审核者。
- 遵循 AGENTS.md：改动经 hook 自动记录，无需手动 dt event。

## 4. 决策点（开工前需用户拍板）

1. **正文展开触发**：仅 `--show-content` 显式标志？还是"查询精确等于 key 路径"时也自动展开？（建议先只做显式标志，自动展开规则容易误伤，后续迭代）
2. **来源锚点格式**：`#spring.cloud.nacos`（裸 key 路径）还是保留 `#section=spring.cloud`？（建议裸 key 路径，更短更直观）
3. **nacos_config 类别是否参与 `--file-type config` 过滤**：yaml 后缀的 Nacos 配置按后缀属 Config 类，按来源属 NacosConfig——建议**来源优先**（dt://nacos/ 一律 NacosConfig），`--file-type config` 不匹配 Nacos 来源，`--file-type nacos` 专属。用户确认此语义。

## 5. 不做的事（本方案边界）

- 不修复 G1/G2/G5/G6（CLI 接通、自检 SocketAddr、扩展名去重、性能）——属 Phase 1 看板。
- 不做全量 config_chunks LLM 回填（CPU 模型 110+ 分钟，另行批准）。
- 不动 Memgraph ConfigKey/ConfigSection 节点结构（kg_bridge 同步逻辑不变）。
- 不改 resource_type 识别（nacos-llm-first 方案的结构化资源检索属后续阶段）。
- 不发布 release。
