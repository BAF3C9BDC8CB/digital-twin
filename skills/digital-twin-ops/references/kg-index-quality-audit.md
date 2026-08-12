# KG 索引质量与使用链路诊断 (im-center, 2026-08-12)

方法论: run_cypher_query 统计 → 抽样对照源码 → 检索入口验证 → 全链路耗时。只诊断不改代码。适用: 任何已索引服务的 KG 质量审计。

## 1. 索引完整性检查 (Cypher)

```cypher
MATCH (n) UNWIND labels(n) AS lbl RETURN DISTINCT lbl ORDER BY lbl          -- 全部标签
MATCH ()-[r]->() RETURN type(r) AS rel, count(*) AS cnt ORDER BY cnt DESC   -- 全部关系
MATCH (p:Project) RETURN p.name, p.language ORDER BY p.name                  -- 项目清单
MATCH (c:Class) WHERE c.project='im-center' RETURN count(c)                  -- 类数
MATCH (m:Method) WHERE m.project='im-center' RETURN count(m)                 -- 方法数
MATCH (a)-[r:BELONGS_TO]->(b) WHERE a.project='im-center'
  RETURN labels(a), labels(b), count(*)                                      -- 归属层级
MATCH (a)-[r:CALLS]->(b) WHERE a.project='im-center'
  RETURN labels(a), labels(b), count(*)                                      -- 调用边
MATCH (c:Class) WHERE c.project='im-center' WITH c.file_path AS fp, count(c) AS n
  WHERE n > 1 RETURN fp, n                                                   -- 重复节点
```

注意: `count{...}` 子查询本环境不支持; 用 `UNION ALL` 拼多个 count。

## 2. im-center 实测基线 (2026-08-12)

- Class 357 / Method 2287 / Entity 55; Project 节点存在(language=java, project_type="微服务 — 消息/通知")
- 层级: Project ←BELONGS_TO— Class/Method; Class -[:CONTAINS]-> Method(2287 全挂); Method→Method CALLS 8903
- **无 Module 节点**(全库 15 个也非 im-center); **无 Service/Config/Server/Event 标签**(配置项以 Entity 存储, 有 summary)
- **无 INDEXED_METHOD/DEPLOYS 关系**(任务假设的关系类型与实际 schema 不符)
- 类数 > 源文件数: 357 类 vs 327 .java 文件 — 内部类被拆成独立 Class 节点(如 TIMImageMsgElement.java → TIMImageMsgElement/ImageMsgContent/ImageInfo 3 节点), 非 bug 但统计口径要知悉

## 3. 内容质量检查

- Class 描述: `MATCH (c:Class) WHERE c.project='im-center' AND (c.summary IS NOT NULL OR c.description IS NOT NULL OR c.llm_analysis IS NOT NULL) RETURN count(c)` → **实测 0, 类描述全空**
- Method comment 覆盖率: 2287 中 460 有 comment(20%), 1827 空(80%)
- **注释错位 bug**(对照源码确认): GroupService 中 `groupMsgGetSimple`/`sendGroupSystemNotification`/`sendGroupMsg` 实际无注释, 但 KG 全部被标成"删除群成员消息"(错误复制自 `deleteGroupMsgBySender`)。验证法: 抽样方法 → `grep -n "public .*(" 源码` 核对行号附近注释归属。
- **双写不一致**: Memgraph 节点无 llm_analysis, 但 dt_search 返回的 llm_analysis 来自 Qdrant payload → 以 dt_search 输出为准, Memgraph 属性查询看不到。

## 4. 检索入口验证结论

| 入口 | im-center 代码命中 | 备注 |
|---|---|---|
| dt_search_kg("im-center getGroupId"/"builder"/"toString"/"GroupService") | ✗ 0 命中, 全是 message-center/med-alliance/copartner 文档噪音 | knowledge 世界不索引 Method/Class |
| dt_search("getGroupId", world=code) | ✓ 5/5 im-center, score 0.95, 带 llm_analysis | 代码检索唯一正确入口 |
| dt_search("GroupService 创建群组", world=code) | ✓ createGroup 命中, llm_analysis 准确 | 0.3s |

CLI 注意: `dt search kg "..."` 非法(报 unexpected argument); 正确 = `dt search "<q>" --world knowledge`。跨项目过滤用 `-p/--project` 参数。

## 5. 链路耗时 (live CLI)

- `dt sense <路径>` 0.2s: indexed, stats 正确, key_entities 按 in_degree(如 builder in-degree 4929)
- `dt search --world code` 0.3s
- `dt sense` 必须传**真实目录路径** `/data/aflmProjects/aflm/uvp-im-center`; 传 "im-center" 会落到 cwd 项目(digital-twin-v2)

## 6. 文档与真实环境脱节(待修 repo 文档)

AGENTS.md / skill/guides/KG-QUERY.md / docs/superpowers/specs/* 中的 `CALL db.index.fulltext.queryNodes("infra_search", ...)` 在 Memgraph 上不可执行(Neo4j 语法 + 索引不存在)。改进方案:
- 需重建索引(dt build): 类级 summary、注释定位 bug、Module 挂载、Memgraph 节点补 llm_analysis
- 需改查询方式: 代码实体一律 dt_search(world=code); dt_search_kg 仅知识/配置; 更新上述文档示例
