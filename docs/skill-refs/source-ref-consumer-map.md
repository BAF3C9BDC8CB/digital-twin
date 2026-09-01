# source_ref 消费方地图

**用途**: 任何改 `source_ref` / `doc_id` / 搜索「来源」显示的变更前必读。

**审计方式**: `rg -n "source_ref" src/`(60 处) + `rg -n "dt://" src/` + mcp 侧核查。

## 结论速览

- 无去重/图扩展/purge 把 source_ref 当身份键 → 改值不破坏这三类功能。
- doc 世界 `source_ref == payload doc_id`,hit.id = `{doc_id}:{block}`(强不变量)。
- kg_bridge config_chunks:**point id = source_ref**,doc_id = `source_ref#0`(存储侧绑定,写侧改动=高风险)。
- MCP(mcp-server.py)纯透传零解析 → schema 兼容,值语义漂移。
- 值级替换必挂 7 处测试断言;显示层替换只挂 search_render 渲染断言。

## A. 生产者(写入 source_ref 值)

| 位置 | 值形态 |
|---|---|
| search_mcp.rs:672,745(doc 世界向量+关键词通道) | `Some(payload.doc_id)` = `dt://doc/{project}/{rel_path}` |
| search_config.rs:251,307(config 世界) | `nacos_source_ref()` = `dt://nacos/{ns}/{group}/{dataId}#{section}`(:11-28) |
| search_config.rs:382 | RankedItem.source_ref 透传 |
| search_config.rs:472(Cypher 回退) | 图节点 `source` 字段(namespace/environment/project) |
| retrieve.rs:793(知识世界 attach_relations) | 最高 confidence 边的 doc_id |
| retrieve.rs:1021(fill_source_refs 回退) | MENTIONED_IN 边的 Document.doc_id(`dt://doc/...`) |
| kg_bridge.rs:334-350(config_chunks 写侧) | `dt://config/{ns}/{group}/{dataId}#{section}`;**point id 同值**,doc_id 加 `#0` |
| search_memory.rs:55 / fusion.rs:100 / retrieve.rs:684,723,2038 / search_config.rs:170 / search_mcp.rs:151,1041 | None 占位(不受影响) |

## B. 读取/消费者

| 位置 | 用途 | 值替换影响 |
|---|---|---|
| search_render.rs:66-67 | render_human `来源: {source_ref}`( 要改的显示行) | 显示层改动点(方案 A) |
| search_render.rs:95-97 | render_json 序列化整个 SearchHit | JSON 值语义出口 |
| search_mcp.rs:904(postprocess_hits) | `file_path.or(source_ref).or(id)` 推断 file_type | 磁盘路径按后缀仍可推断;nacos 前缀分支失效 |
| search_mcp.rs:883(infer_file_type_pub) | `dt://nacos/` 前缀 → NacosConfig(来源优先于后缀) | 值替换 nacos 来源 → 标签/过滤丢失 |
| retrieve.rs:1141-1145 | 知识命中 file_type 推断自 source_ref | 同上 |
| mcp/mcp-server.py:252-262,583-598 | run_cmd 透传 `dt search --json`,零解析 | 契约 schema 不变 |

## C. 身份键对照(都不依赖 source_ref)

- 去重:`fusion.rs:37,65` RRF 键 `{world}:{id}`;`search_config.rs:372` title;`:456` name;`search_mcp.rs:703-724` hit.id;`retrieve.rs:660-731` merge_candidates 按 business_id
- purge:`consolidate.rs:557-588` purge_document 按 payload/Memgraph 侧 **doc_id**(RELATES 边/MENTIONED_IN/Document 节点/doc_chunks 点);`kg_bridge.rs:365` 按 namespace+data_id
- 图扩展:`kg_bridge.rs:431,617,678` 用 elementId/element_id

## D. 三档风险分层

1. **仅显示层(render_human `来源:` 行)= 低**:JSON/MCP 不变;「来源」是 契约级元素,渲染断言 search_render.rs:164-168/193-198/232 需同步;JSON 与人类输出不一致。
2. **值级替换 SearchHit.source_ref = 中高**:破坏 doc 世界 id↔source_ref 不变量;`dt://nacos/` file_type 判定失效;7 处值级测试断言必挂;虚拟来源(dt://nacos|jenkins|entity|experience|knowledge|config|db)无磁盘路径。
3. **存储写侧改 kg_bridge.rs:334-350 = 高**:source_ref 同时是 Qdrant point id 和 doc_id 前缀 → 改点身份 → 旧点残留、按 doc_id 的 purge/过滤失效。

## E. 值级测试断言(改值必挂)

- search_mcp.rs:1340-1342 `hit.source_ref == Some("dt://doc/offen-pay/pay-design.md")`
- search_config.rs:601-602 `Some("dt://nacos/public/DEFAULT_GROUP/app.yaml#spring")`
- search_config.rs:636-639 `Some("dt://nacos/test/CUSTOM_GROUP/uvp-common.yaml#spring.cloud")`(+ 不含 environment)
- search_config.rs:677-711 `Some("dt://nacos/public/DEFAULT_GROUP/config#spring.cloud")`
- retrieve.rs:1732,1842-1850 source_ref = "d1"/"d-c"(边 doc_id 回填)、k.source_ref.is_none()
- search_render.rs:164-168/193-198/232 渲染串断言(改渲染即挂)

值无关(安全):search_mcp.rs:998(`"source_ref":null`)、tests/s5_knowledge_search.rs:364(is_some)、build_service.rs:340(fixture)、fusion.rs:100(None)。

## F. config.yaml 映射坑(拼磁盘全路径时)

- `projects = [{base, items: [name | {display: 物理目录}]}]`:display 名≠物理目录名(如 copartner-h5→copartner/copartner-h5)
- 同名项目可出现在多个 base(歧义)
- knowledge 世界 source_ref 的 project 段可能不在 config.yaml(test-pipeline / test/fixtures)→ 映射失败需回退原值
- code 世界 SearchHit 只有 file_path(相对)无 project 字段 → 拼全路径需给 SearchHit 加 project(动 JSON/MCP 契约)或从请求侧传入映射表

## G. 推荐形态

仅显示层映射,或新增 `display_source` 字段(JSON 同时给 dt:// 规范值 + 磁盘路径展示值),`source_ref` 保持 dt:// 不动。映射失败/虚拟来源保留原值。
