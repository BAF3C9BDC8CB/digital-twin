# KG 检索质量验证 & dt_search_kg 代码检索盲区 (2026-08-12)

im-center 检索质量验证实测。早期核心结论: **`dt_search_kg` 对代码(Class/Method)检索失效**, 当时根因判定为检索范围配置问题。**⚠️ 2026-08-12 后续会话实测推翻**: `dt_search_kg(world=code, project=im-center, ...)` 直接命中 code 世界方法(`MessageController.msgWithdraw`/`GroupService.groupMsgRecall`/`MessageRecordMongoService.groupMsgRecallUpdate`, hits 的 project=im-center)。机制未复查(mcp-server.py 可能已按下方"改进建议 #1"加了参数, 或旧结论误把"不带 world 的默认行为"当硬编码)。**现状正确姿势: 纯代码问题首查 `dt_search_kg(world=code, project=<目标>)`; 不带 world 时默认仍走 knowledge 世界, 会命中他项目知识实体(跨项目噪音)**。诊断方法论与 Qdrant 对账部分仍然有效, 继续用于区分 4 类失败原因。

## 根因(早期版本事实, 现已被上面实测部分推翻)

源码 `mcp-server.py`(`if name == "dt_search_kg"`)(早期版本):

```python
cmd = [DT_BIN, "search", query, "--world", "knowledge", "--limit", str(limit), "--json"]
```

- 早期版本 `dt_search_kg` = `dt search --world knowledge`, **无 `--project` 参数**。
- `dt_search` 支持 `--world` 和 `--project`, 曾是代码检索的唯一正确入口。
- im-center 实测(早期): 10/10 条 dt_search_kg 查询 top5 全被 message-center(友盟推送)知识实体抢占, im-center 零命中 → 正确率 0%。该现象在**不带 world 参数**时仍会出现。
- 对照: `dt_search(world=code, project=im-center)` 查 "发送单聊文本消息" 精准命中 `Message.sendMsg`。

## 诊断方法论: 区分 4 类失败原因(对照实验法)

1. **dt_search_kg 逐条跑查询**, 记录 top3 命中的 实体名/类型/project/hop + rerank 分数。
2. **对照实验**: 同一查询换 `dt_search(world=code, project=<目标>)`。
   - 命中目标方法 → 向量索引正常, 失败原因是 **world 范围配置/跨项目噪音**;
   - 仍不命中 → 才是 **向量失效/索引缺失**, 再查 Qdrant 对账。
3. **Qdrant 对账**(python, 注意端口):
   ```python
   from qdrant_client import QdrantClient
   c = QdrantClient(url='http://localhost:6333')   # REST 是 6333!config.yaml 写 6334 是 gRPC
   for col in c.get_collections().collections:
       print(col.name, c.get_collection(col.name).points_count)
   # 按 project 计数:
   for col in ['code_methods','kg_nodes']:
       cnt = sum(1 for p, _ in c.scroll(col, limit=1000, with_payload=True) if (p.payload or {}).get('project')=='im-center')
   ```
   实测集合: `code_methods` 13374 / `kg_nodes` 790 / `doc_chunks` 119; im-center = code_methods 2287(与 Memgraph 完全一致)+ kg_nodes 55(全是配置实体)。
4. **Cypher 完整性抽查**:
   - 节点: `MATCH (n) WHERE n.project='im-center' RETURN labels(n), count(*)`
   - 关系端点语义: `MATCH (a)-[r:TYPE]->(b) WHERE a.project='im-center' RETURN labels(a), labels(b), count(*)`
   - 关键类/方法存在性: 按 name CONTAINS 抽查。

## 图 schema 事实(验证过的)

- `CONTAINS`: **Class → Method**(2287), 不是 Project→Class。
- `BELONGS_TO`: Method→Project(2287) + Class→Project(357)。**Project 节点只有入边, 无出边**; 从 Project 出发遍历不了, 必须从 Class/Method 出发。
- im-center: 2287 Method + 357 Class + 55 Entity(全部是 Nacos/Spring/yml 配置项, **无业务实体**)+ 4 Document。
- 无 Service/Module/Package 标签 → 缺服务级视图。
- 噪音分数特征: message-center 污染实体 rerank 普遍 <0.5; 有效命中(code world)>0.66 → 可作低分降级提示阈值参考。

## 改进建议(给 dt_search_kg 的)

1. 加 `world` + `project` 参数(对齐 dt_search), 默认多 world 或 code。
2. 对语义重合项目(message-center ↔ im-center)做 project 白名单/别名映射。
3. 低 rerank 分(<0.5)时提示"结果可能不相关"。
4. 项目缺知识层时(Entity 全是配置项)提示补业务文档再 dt_build 抽取。

## 报告模板(中文)

检索测试表(查询→返回数→top3 命中→相关?→✓/⚠/✗) + 正确率统计 + 失败原因分类(索引缺失/查询词不当/跨项目噪音/向量失效/world 范围配置) + 改进建议。判定标准: ✓=top 命中含目标实体, ⚠=部分相关, ✗=不相关/空。
