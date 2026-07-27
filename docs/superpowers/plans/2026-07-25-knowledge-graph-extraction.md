# 知识图谱信息抽取 — LLM-Only 完整方案

> **目标：** 从知识文档（`test/fixtures/knowledge/*.md`）中自动抽取实体和关系，写入 Memgraph 知识图谱，形成可导航的知识网络。
>
> **思路：** 不用 HanLP，只用 LLM。一次 prompt 输出结构化 JSON，解析后写 Cypher。

---

## 一、系统 Prompt 模板

```
你是一个知识图谱抽取引擎。你从技术文档中提取结构化知识。

## 实体类型
- Tool          — 工具/平台 (Docker, Kubernetes, Nacos, Jenkins)
- Database      — 数据库 (MySQL, Redis, PostgreSQL, Elasticsearch)
- Service       — 微服务 (doctor-center, pay-center, order-service)
- ConfigKey     — 配置项 (TZ, spring.datasource.url, server.port)
- ConfigFile    — 配置文件 (bootstrap.yml, application.yml, Dockerfile)
- Pitfall       — 踩坑/问题
- Solution      — 解决方案
- Symptom       — 症状表现 (数据库时间差8小时, 配置不刷新)
- Library       — 库/框架 (Spring Cloud, MyBatis, tree-sitter)

## 关系类型
- CAUSES        — A 导致 B (配置不当 CAUSES 服务异常)
- SOLVED_BY     — 问题被某方案解决 (Pitfall SOLVED_BY Solution)
- HAS_CONFIG    — 实体有某配置 (MySQL HAS_CONFIG TZ)
- DEPENDS_ON    — 依赖 (Service DEPENDS_ON Database)
- RELATED_TO    — 相关联
- OCCURS_IN     — 问题发生在某环境 (Pitfall OCCURS_IN Tool)

## 输出格式
必须是合法的 JSON，不要 markdown 代码块包裹，不要额外文字。

{
  "entities": [
    {
      "id": "唯一标识（英文小写加连字符）",
      "type": "实体类型（上方枚举之一）",
      "name": "显示名称",
      "props": { "version": "8.0", "severity": "high" }
    }
  ],
  "relations": [
    {
      "source": "实体id",
      "target": "实体id",
      "type": "关系类型（上方枚举之一）"
    }
  ]
}

## 抽取规则
1. 每个实体只出现一次，多段文本提到同一实体时复用 id
2. 不确定的属性不填，不要编造
3. props 只填文档中明确提到的信息
4. 如果文档内容为空或不相关，返回 {"entities":[], "relations":[]}
```

---

## 二、JSON Schema（Rust 侧解析）

```rust
/// LLM 返回的实体抽取结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeExtraction {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: String,      // Tool | Database | Service | ConfigKey | ...
    pub name: String,
    #[serde(default)]
    pub props: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub rel_type: String,         // CAUSES | SOLVED_BY | HAS_CONFIG | ...
}
```

---

## 三、Cypher 写入逻辑

```rust
/// 将 LLM 抽取结果写入 Memgraph
async fn write_extraction_to_graph(
    graph: &dyn GraphRepository,
    project: &str,
    file_path: &str,
    extraction: &KnowledgeExtraction,
) {
    // ── 第 1 步：写实体节点 ──
    for entity in &extraction.entities {
        let entity_id = format!("dt://entity/{}/{}", project, entity.id);
        let mut params = HashMap::new();
        params.insert("id".into(), json!(entity_id));
        params.insert("name".into(), json!(&entity.name));
        params.insert("type".into(), json!(&entity.entity_type));
        params.insert("project".into(), json!(project));

        // 动态设置 props
        let mut set_clauses = Vec::new();
        for (k, v) in &entity.props {
            let key = format!("n.{}", k);
            set_clauses.push(format!("{} = ${}", key, k));
            params.insert(k.clone(), json!(v));
        }
        let set_str = if set_clauses.is_empty() {
            String::new()
        } else {
            format!(", {}", set_clauses.join(", "))
        };

        let cypher = format!(
            "MERGE (n:ExtractedEntity {{entity_id: $id}})
             SET n.name = $name, n.type = $type, n.project = $project{}
             RETURN elementId(n)",
            set_str
        );
        let _ = graph.write_query(&cypher, params).await;
    }

    // ── 第 2 步：关联到源 Document ──
    for entity in &extraction.entities {
        let entity_id = format!("dt://entity/{}/{}", project, entity.id);
        let mut params = HashMap::new();
        params.insert("id".into(), json!(entity_id));
        params.insert("file_path".into(), json!(file_path));
        params.insert("project".into(), json!(project));
        let _ = graph.write_query(
            "MATCH (e:ExtractedEntity {entity_id: $id})
             MATCH (d:Document {file_path: $file_path, project: $project})
             MERGE (e)-[:EXTRACTED_FROM]->(d)",
            params,
        ).await;
    }

    // ── 第 3 步：写关系 ──
    for rel in &extraction.relations {
        let source_id = format!("dt://entity/{}/{}", project, rel.source);
        let target_id = format!("dt://entity/{}/{}", project, rel.target);
        let mut params = HashMap::new();
        params.insert("source_id".into(), json!(source_id));
        params.insert("target_id".into(), json!(target_id));
        let cypher = format!(
            "MATCH (a:ExtractedEntity {entity_id: $source_id})
             MATCH (b:ExtractedEntity {entity_id: $target_id})
             MERGE (a)-[:{}]->(b)",
            rel.rel_type
        );
        let _ = graph.write_query(&cypher, params).await;
    }
}
```

**节点标签设计：**
- 统一用 `:ExtractedEntity` 标签，通过 `type` 属性区分具体类型
- 好处是不需要为每种实体类型都创建不同标签，查询时直接用 `WHERE n.type = 'Tool'` 过滤
- 如果需要性能优化，后续可以为高频类型加次级标签

**关系类型直接用 LLM 输出的字符串：**
- 好处是灵活，LLM 可以根据上下文选择合适的类型
- 坏处是可能不一致（比如有时 `CAUSES` 有时 `LEADS_TO`）——通过 system prompt 规范枚举值来避免

---

## 四、集成到现有 Pipeline

### 4.1 复用现有的 LlmClientProcessor

`src/application/pipeline/processors/llm_client.rs` 已经支持 `.md` 文件（line 69），并且有 prompt 选择逻辑。只需要：

**新增 prompt 模板** `config/prompts/kg_extraction.yaml`：
```yaml
system: |
  你是一个知识图谱抽取引擎。从以下技术文档中提取结构化知识。
  ...
```

**LlmClientProcessor 已经做的事情：**
1. ✅ 接收文件内容
2. ✅ 调用 LLM 获取回复
3. ✅ 输出 `ProcessorOutput` 给下游

### 4.2 新建 Store 步骤

新增 `src/application/pipeline/processors/kg_store.rs`：

```rust
/// 将 LLM 抽取的实体/关系写入 Memgraph
pub struct KgStoreProcessor {
    graph: Arc<dyn GraphRepository>,
}

#[async_trait]
impl Processor for KgStoreProcessor {
    fn name(&self) -> &str { "kg_store" }
    fn priority(&self) -> i32 { 80 }  // 在 LLM 之后运行

    fn matches(&self, file_path: &Path) -> bool {
        // 只处理知识文件
        let path = file_path.to_string_lossy();
        path.contains("/knowledge/")
    }

    async fn execute(&self, ctx: &PipelineContext) -> Result<ProcessorOutput, DtError> {
        // 1. 从 PipelineContext 获取 LLM 的输出
        let llm_output = ctx.get_processor_output("llm")?;
        let response_text = llm_output.get("response")?;

        // 2. 解析 JSON
        let extraction: KnowledgeExtraction = serde_json::from_str(&response_text)?;

        // 3. 写入 Memgraph
        write_extraction_to_graph(&self.graph, &ctx.project, &ctx.file_path, &extraction).await;

        Ok(ProcessorOutput::default())
    }
}
```

### 4.3 注册到 ProcessorRegistry

`src/application/pipeline/registry.rs` 中注册新 processor：

```rust
registry.add(Arc::new(KgStoreProcessor::new(graph.clone())));
```

### 4.4 Pipeline 执行流

```
知识文档 (.md)
  │
  ├─→ [Document Parser]  解析 frontmatter、正文
  ├─→ [Chunk Processor]  切片 → Qdrant _knowledge 集合（现有）
  ├─→ [LLM Processor]    prompt: kg_extraction.yaml → JSON
  └─→ [KgStore Processor] 解析 JSON → Cypher → Memgraph
```

---

## 五、查询示例

### 搜指定实体的关联知识

```cypher
// 查 MySQL 相关的所有踩坑和解决方案
MATCH (e:ExtractedEntity {type: "Database", name: "MySQL"})
MATCH (e)<-[:EXTRACTED_FROM]-(d:Document)
OPTIONAL MATCH (pitfall:ExtractedEntity {type: "Pitfall"})-[:EXTRACTED_FROM]->(d)
OPTIONAL MATCH (sol:ExtractedEntity {type: "Solution"})-[:EXTRACTED_FROM]->(d)
RETURN d.file_path, collect(DISTINCT pitfall.name) AS pitfalls,
       collect(DISTINCT sol.name) AS solutions
```

### 查知识文档的完整图谱

```cypher
// 找一篇文档中所有实体和关系
MATCH (d:Document {file_path: "fixtures/knowledge/docker-mysql-timezone.md"})
MATCH (e:ExtractedEntity)-[:EXTRACTED_FROM]->(d)
OPTIONAL MATCH (e)-[r]->(other:ExtractedEntity)
RETURN e.name AS entity, e.type AS type,
       type(r) AS relation, other.name AS related
```

### 查踩坑的根因链路

```cypher
// 从 ConfigKey 出发，查找导致的问题链
MATCH (cfg:ExtractedEntity {type: "ConfigKey"})
MATCH path = (cfg)-[:CAUSES*1..3]->(pitfall:ExtractedEntity {type: "Pitfall"})
RETURN cfg.name, pitfall.name,
       [r IN relationships(path) | type(r)] AS chain
```

---

## 六、与现有 @knowledge 注释的关系

| 维度 | 现有 `@knowledge` 方式 | LLM 抽取方式 |
|------|----------------------|-------------|
| 数据结构 | 固定字段 concept/pitfall/experience | 任意实体类型和关系 |
| 覆盖度 | 需要人工手动写注释 | 自动从全文抽取 |
| 粒度 | 粗，一段文档一个 concept | 细，多实体多关系 |
| 学习成本 | 要记 `@knowledge` 格式 | 自然语言写文档即可 |
| 运行成本 | 无 | 每次构建走 LLM |

**建议两者并存：**
- `@knowledge` 注释作为快速标记方式，适合只想写一两行 metadata
- LLM 抽取作为深度分析方式，需要写完整文档时自动生成知识图谱
- 在 pipeline 中，LLM 抽取结果可以和 `@knowledge` 注释的结果合并写入

---

## 七、注意事项

### 7.1 LLM JSON 解析健壮性

LLM 可能输出不规范的 JSON（多余注释、markdown 包裹等）。需要加一层容错：

```rust
fn parse_llm_json(text: &str) -> Result<KnowledgeExtraction, DtError> {
    // 1. 尝试去掉 ```json ... ``` 包裹
    let cleaned = text.trim()
        .strip_prefix("```json").or_else(|| text.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .unwrap_or(text);

    // 2. 尝试直接解析
    serde_json::from_str(cleaned)
        // 3. 失败时尝试用 regex 提取 JSON 对象
        .or_else(|_| {
            let re = regex::Regex::new(r"\{[\s\S]*\}").unwrap();
            re.find(cleaned)
                .and_then(|m| serde_json::from_str(m.as_str()).ok())
                .ok_or_else(|| DtError::General("LLM response is not valid JSON".into()))
        })
}
```

### 7.2 实体 ID 冲突

同一概念在不同文档中可能出现（如 "MySQL" 在多个文档中出现）。用 `name + type` 哈希做 ID 可以实现去重：

```rust
fn make_entity_id(project: &str, type_name: &str, name: &str) -> String {
    let hash = {
        use sha2::{Sha256, Digest};
        let mut h = Sha256::new();
        h.update(format!("{}:{}:{}", project, type_name, name));
        format!("{:x}", &h.finalize()[..8])
    };
    format!("dt://entity/{}/{}", project, hash)
}
```

### 7.3 已分析文档跳过

跟 Phase 2 一样，用 SQLite 标记已分析的文档。LLM 调用成本高，必须做增量跳过。

### 7.4 针对 `test/fixtures/knowledge/` 的最小化实现

如果不想动整个 pipeline，最简实现是：

在 `process_documents()` 中现有嵌入步骤之后加一段：

```rust
// 只在 knowledge/ 目录的文件上执行 LLM 抽取
if is_knowledge_file && siliconflow.is_some() {
    let client = siliconflow.as_ref().unwrap();
    let result = client.chat(KG_SYSTEM_PROMPT, &parsed.content, 0.1, 2000).await?;
    if let Ok(extraction) = serde_json::from_str::<KnowledgeExtraction>(&result) {
        self.write_extraction_to_graph(graph, project, &parsed.rel_path, &extraction).await;
    }
}
```

这样改动最小，只在 `pipeline.rs` 中加 ~30 行代码。
