# source_ref 语义盘点 + 「搜索结果显示磁盘全路径」架构评审 (2026-08-11)

背景: 用户提案把搜索结果显示的 `来源: dt://doc/pay-center/xxx.md` 替换为磁盘全路径
(`base + 映射目录 + rel`, 用 config.yaml projects 映射生成), 要求先架构评审。
**评审结论: 可行, 但只能做在展示层 (render_hit 渲染时只读翻译); 改 `SearchHit.source_ref`
字段值 (数据层) 被否决** — 会破坏 4 类功能。本文件为证据 + 实施清单, 评审后待实施。

## source_ref 的全部消费点 (改字段值的破坏面)

| 用途 | 代码位置 | 换值影响 |
|---|---|---|
| 展示"来源:"行 | search_render.rs:66-71 (`来源: {sr} [hop=N]`); render_human 仅被 build.rs:807 handle_search 调用 | 正是要改的 |
| **file_type 推断** | search_mcp.rs:901-905 (postprocess_hits: file_path.or(source_ref).or(id)); infer_file_type_pub:878-892 **`dt://nacos/` 前缀硬编码→NacosConfig**; retrieve.rs:1141-1145 知识 hit 同源推断 | **破坏**: 换成磁盘路径后前缀判定失效 → NacosConfig 分类 + `--file-type nacos` 过滤全错 |
| **--json MCP 契约** | render_json:95-97 原样序列化; mcp/mcp-server.py dt_search 子进程 `dt search --json` 消费 source_ref | **破坏**: MCP 侧失去 dt:// 身份 |
| gRPC 输出 | build_service.rs SearchHit→proto 映射 (含 source_ref, 测试 L340) | 数据层改动连带污染 |
| 知识溯源语义 | retrieve.rs fill_source_refs:984-1021 — source_ref = 最高 confidence 边 doc_id (RELATES/MENTIONED_IN 回退) | 换值后溯源身份丢失 |
| 去重 | fusion.rs rrf_hits 键 `{world}:{id}`:65; doc 世界 seen 集合按 id | 不依赖, 无影响 |
| purge_document | pipeline.rs:165 用构建期 make_document_id(project, rel), 不读 SearchHit | 无影响 |

## URI 类型 → 磁盘路径可翻译性

- `dt://doc/{alias}/{rel}` (make_document_id = `dt://doc/{project}/{path}`, src/domain/id.rs:50) — **唯一可翻译**; 第一段 = 项目别名
- `dt://nacos/{ns}/{group}/{dataId}#{key}` (search_config.rs nacos_source_ref:11-28) — 远程, 无磁盘路径, 保持原样
- `dt://config/{ns}/{group}/{dataId}#{section}` (kg_bridge.rs:334-336 遗留形态, doc_id=source_ref#0) 与 build_config_index.py 的 `dt://config/...` — 无磁盘映射, 保持原样
- `dt://entity/{project}/{Type}/{name}` (consolidate.rs entity_id_for:113-120) — KG 节点, 无磁盘路径
- `dt://method/...` (legacy) — 无磁盘路径
- 别名不在 config / 解析失败 → 回退显示原 dt:// 串

## code 世界项目名缺口 (评审第 4 问)

- `SearchHit` 结构 (search_mcp.rs:241-301) **没有 project 字段**
- 但 Qdrant code_methods payload **有 `project`**: build/pipeline.rs:253/760 写入; search_code 已按 payload project 过滤 (U-D6, L415 起); sense/mod.rs:145-147 也按 payload project scroll
- `hit_from_payload` (search_mcp.rs:108-185) 未读 project → 实施需给 SearchHit 加 `project: Option<String>` (`#[serde(default)]`, legacy JSON 兼容), hit_from_payload 填充, 同步 ~8 处 SearchHit 构造点 (search_memory.rs:55, fusion.rs 测试, search_mcp 测试, retrieve.rs:1146, search_config.rs:170, build_service.rs 等 — 逐个 patch, 勿正则批量插, 见 SKILL.md 教训)

## 接线缺口 (实施前必须补)

- main.rs Search 命令分支 (L1195-1218) 只传 graph/vector 给 handle_search; **handle_search 收不到 config**
- 对比 handle_sense (main.rs:1226) 已传 `resolve_project_paths` 结果 → Search 分支需同样 `load_config()` + resolver 透传

## config.yaml 实测 (2026-08-11)

- 7 个 base / 65 个别名 / **当前无跨 base 重复别名** (解析器仍按"精确匹配 + 失败回退原 URI"写, 防漂移)
- mapping 形式 `{alias: rel_dir}`: `pay-center → /data/aflmProjects/unimportant/uvp-pay-center` 与用户预期一致
- mapping 值可含多段相对路径: `cashier: pay/offenpay-ui/offenpay-ui-cashier`, `copartner-h5: copartner/copartner-h5`
- 同一目录可有多别名 (copartner / copartner-h5 → 同一目录), 按别名解析不受影响
- `test-pipeline` (duplicate-hits 历史问题) 不在 config → dt://doc/test-pipeline/... 显示原 URI, 可接受

## 实施清单 (展示层方案, 5 步)

1. main.rs Search 分支: `load_config()` + `resolve_project_paths` → 新参数传 handle_search
2. build.rs handle_search: 透传 resolver → `render_human(&result, show_content, &resolver)`
3. search_render.rs: 仅对 `dt://doc/` 前缀翻译 source_ref; file_path 分支在 `h.project` 可解析时前缀 base; 其余原样; **不 stat 文件存在性** (映射即权威)
4. search_mcp.rs: SearchHit 加 `project` 字段 + hit_from_payload 填充 + 同步构造点
5. 测试: search_render.rs 加"dt://doc 翻译成功 / nacos+entity 不翻译 / 未知别名回退"用例; golden 回归确认来源行格式变化不影响断言

明确不做: 不改 JSON、不改 Qdrant/Memgraph 数据、无需重建索引 (纯展示层); MCP 保持 dt:// 原值
(若未来 MCP 也要全路径, 另加可选 `resolved_path` 字段, 默认不加)。

## 工具小坑

read_file 把 src/main.rs 判为 binary (文件含非 UTF-8 字节), 读取用
`LC_ALL=C sed -n 'A,Bp' src/main.rs | cat -v`。
