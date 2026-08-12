# 统一搜索渲染 — 已验证事实与代码定位 (2026-08-07)

支撑 SKILL.md "统一搜索渲染与 Nacos 接入契约" 一节的实测细节。方案主文档在仓库内:
`/data/myProject/digital-twin-v2/docs/plans/unified-pipeline-search-plan-2026-08-07.md`。

## config_chunks 数据层实测 (scroll 验证)

- 总量 1607 点。payload 键集合固定: `config_type, data_id, environment, group, key_count, namespace, section_name, source_type, text`。
- `environment` 全部为空串 `""`; `source_type` 全为 `"config_chunk"`; **无 `resource_type` 字段**。
- `namespace` 值为名称 (`test`/`local_test`), 不是 UUID; group 多为 `DEFAULT_GROUP`。
- `text` 字段逐字符保留原始 YAML: 缩进、行内注释 (`#命名空间 代指某个环境`)、双井注释 (`## nacos.newoffen.net:8848`)、对齐空格全部原样 — 即"正文保真"在数据层已成立, 只需渲染层不压行。
- 痛点量化: `mysql` 全文匹配 127 点, 其中 10 点是 pagehelper/helper-dialect 弱匹配, 101 点含 `jdbc:mysql` — nacos-llm-first 方案 §1 的痛点真实存在 (resource_type 识别属后续阶段, 不在本次收敛范围)。

## 代码定位 (实施时的精确接缝)

| 改动 | 位置 |
|---|---|
| FileCategory 枚举/slug/label/suffix_map/resolve_file_types | `src/domain/file_type.rs` (唯一文件, 含单测) |
| file_type 推断入口 | `src/application/context/search_mcp.rs:878` `infer_file_type_pub`; 填充入口 `postprocess_hits` :890 (回退链 file_path→source_ref→id) |
| 渲染三分支 (Method=分析/Doc=原文/Config*=分析+无条件正文) | `src/interfaces/cli/search_render.rs` `render_hit` ~L16-65 |
| 硬编码摘要 (待删) | `src/application/context/search_config.rs:123` `config_purpose_summary`, 调用点 :242/:298 |
| nacos source_ref 构造 (environment 假数据在此) | search_config.rs ~L240 (动态) 和 ~L296 (硬编码 `dt://nacos/test/public/DEFAULT_GROUP/config#section=...`) |
| Method 的 llm_analysis 真实形态 | `用途：...\n逻辑：...` (code_methods payload 实测), 配置 chunk 对齐此契约 |

## 决策背景

- 用户原话: "和普通的项目、目录构建一样的处理方式, 只不过可能多一些前置处理或者特定节点的处理" + "整体就只有一个核心的处理逻辑"。
- 三个澄清问题 (正文展开方式/锚点格式/过滤语义) 用户未响应, 按推荐默认: 仅 `--show-content` 显式展开、裸 key 锚点、来源优先过滤。
- qwen3.5 仅 CPU 模式 (~40s/条): T3 的 LLM 验证限 3 条样本, 禁止全量回填 config_chunks; LLM 不可用时 mock 验证代码路径。
- 工作区开工时有 7 个未提交修改 (fusion/search_config/search_mcp/search_memory/retrieve/search_render/build_service) + 未跟踪 docs/plans/ — T0 已归位为基线 commit, 后续 worker 不得丢弃。
