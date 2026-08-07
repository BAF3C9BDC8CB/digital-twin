# Nacos 配置 LLM-first 语义分块与结构化搜索方案（草案）

> 状态：方案评审稿，仅供决策；本文件不代表已开始实施。
>
> 当前阶段不修改业务代码、不清理 Qdrant/Memgraph、不删除已有配置数据、不发布新 release。

## 1. 背景与目标

当前 Digital Twin 已能够从 Nacos 同步配置，并在 `config_chunks` 中进行配置搜索，但存在以下问题：

- 配置分块主要依赖 YAML/Properties 的固定规则；
- `warehouse mysql` 这类查询可能返回 `pagehelper.helper-dialect: mysql` 等弱相关结果；
- 配置正文和用于 embedding 的规范化文本没有严格分离；
- namespace、environment、group、dataId 的语义尚未完全统一；
- MongoDB 等资源类型识别不完整；
- 部分大配置超过 Embedding 服务输入上限；
- CLI 默认输出过长，且需要区分人类格式与 JSON 格式；
- 本地目录扫描、`dt build`、代码索引和文档索引必须保持原有行为。

目标：

1. 以 Nacos 原始正文为事实来源；
2. 允许任意配置格式，不依赖固定 YAML/Properties 分块作为唯一方案；
3. 由 LLM（未来优先使用本地 LLM）判断语义区块、配置用途和资源类型；
4. 正文由系统按原文位置截取，保持换行、缩进、注释、空格和配置值；
5. 支持 MySQL、Redis、MongoDB、Kafka 等资源的结构化检索；
6. 查询 `warehouse mysql` 时优先满足 dataId/service 与 MySQL 类型的联合条件；
7. 不影响本地项目目录和既有 `dt build` 逻辑；
8. 支持失败重试、版本化、回滚和完整验收。

## 2. 非目标与安全边界

本方案第一阶段不做：

- 不修改本地项目源文件；
- 不改变 `WalkDir`、本地文件扫描、代码解析和文档扫描；
- 不把 `dt://nacos/...` 纳入本地目录统计；
- 不执行全库清理、删除或强制重建；
- 不把含密码/Token 的 Nacos 原文发送到外部 LLM；
- 不默认发布新 release；
- 不让 LLM 直接生成或改写配置正文。

重要安全说明：用户要求本地搜索可以保留密码，但“原文可搜索”不等于“原文可以发送给外部模型”。原文存储、向量输入、LLM 输入、日志、备份和 CLI 展示需要分别定义边界。

## 3. 总体架构

```text
Nacos API
  │
  ▼
Nacos 原文采集器
  │  namespace_id / namespace_name / environment / group / data_id / raw_content
  ├──────────────────────────────┐
  ▼                              ▼
事实数据层                       分析层
raw_content + content_hash        LLM-first 语义分析
  │                              │
  │                              ▼
  │                        JSON Schema 校验
  │                              │
  │                              ▼
  │                        原文位置校验/截取
  │                              │
  └──────────────┬───────────────┘
                 ▼
       ConfigChunk / ResourceMetadata
                 │
        ┌────────┴────────┐
        ▼                 ▼
 Qdrant config_chunks   Memgraph / 结构化索引
        │                 │
        └────────┬────────┘
                 ▼
       config 结构化搜索 + 语义兜底
                 │
        ┌────────┴────────┐
        ▼                 ▼
  紧凑人类输出          --json 机器输出
```

本地文件链路保持独立：

```text
本地目录 → dt build → code_methods/doc_chunks
```

## 4. 数据模型设计

### 4.1 配置事实数据

建议在现有字段基础上新增，不删除旧字段：

```json
{
  "source_type": "nacos",
  "environment": "test",
  "namespace_id": "af6d04ec-...",
  "namespace_name": "test",
  "group": "DEFAULT_GROUP",
  "data_id": "uvp-warehouse-saas",
  "config_type": "yaml",
  "raw_content": "原始 Nacos 正文，逐字符保留",
  "content_hash": "sha256...",
  "source_ref": "dt://nacos/test/af6d04ec-.../DEFAULT_GROUP/uvp-warehouse-saas"
}
```

字段约定：

- `namespace_id`：Nacos API 使用的 tenant/namespace ID；
- `namespace_name`：展示名称，不作为唯一标识；
- `environment`：运行环境，例如 test/prod；
- `data_id`、`group`、`namespace_id`、`environment` 共同参与唯一定位；
- `raw_content`：唯一事实正文；
- `text`：允许作为 embedding/检索文本，但不能替代 `raw_content`。

### 4.2 语义分块

```json
{
  "chunk_id": "稳定 ID",
  "section_name": "spring.datasource",
  "purpose": "配置应用的 MySQL 数据源连接和连接池参数",
  "resource_type": "mysql",
  "resource_role": "datasource",
  "service": "uvp-warehouse-saas",
  "start_line": 1,
  "end_line": 8,
  "start_offset": 0,
  "end_offset": 312,
  "raw_content": "从原文按位置截取的完整区块",
  "embedding_text": "用于向量化的分析文本",
  "chunk_strategy": "llm",
  "analysis_status": "ready",
  "analysis_model": "local-model-name",
  "analysis_prompt_version": "v1"
}
```

正文获取规则：

> LLM 只返回区块边界和结构化语义；系统根据 `start_line/end_line` 或字符 offset 从 Nacos 原文截取正文。禁止使用 LLM 改写后的文本作为正文。

## 5. LLM-first 分块契约

### 5.1 LLM 输入

输入包括：

- dataId、group、namespace 等非秘密元数据；
- 原始配置正文（仅允许本地模型，或经过明确脱敏后才允许外部模型）；
- 严格的系统提示词；
- JSON Schema。

### 5.2 LLM 输出

只允许返回结构化 JSON：

```json
{
  "format": "yaml|properties|json|xml|text|unknown",
  "chunks": [
    {
      "name": "spring.datasource",
      "purpose": "配置应用的 MySQL 数据源",
      "resource_type": "mysql",
      "resource_role": "datasource",
      "start_line": 1,
      "end_line": 8,
      "confidence": 0.96,
      "keys": ["url", "username", "password", "driver-class-name"]
    }
  ]
}
```

### 5.3 必须校验

- JSON 可解析；
- Schema 完整；
- 行号从 1 开始且不越界；
- offset 与行号一致；
- chunk 不重叠，或重叠必须显式标记；
- chunk 至少包含一个有效原文字符；
- resource_type 属于白名单；
- `raw_content` 必须能从原文逐字符复原；
- LLM 返回 Markdown、解释文本或非法 JSON 时进入 `pending/retry/failed`。

### 5.4 失败状态

```text
pending → processing → ready
                    ├→ retrying → ready
                    └→ failed
```

LLM 失败不能阻塞 Nacos 原文入库，也不能删除旧版本可用数据。

## 6. 资源类型识别

第一阶段支持：

| 类型 | 识别信号 |
|---|---|
| mysql | `jdbc:mysql://`、MySQL driver、datasource URL、MySQL 专属字段 |
| postgresql | `jdbc:postgresql://`、PostgreSQL driver |
| redis | `redis://`、`spring.redis`、`spring.data.redis`、redis host/port |
| mongodb | `mongodb://`、`mongodb+srv://`、`spring.data.mongodb`、mongo URI |
| kafka | bootstrap servers、Kafka 配置前缀 |
| rabbitmq | RabbitMQ host/URI/配置前缀 |
| elasticsearch | Elasticsearch URI/host/配置前缀 |

资源识别结果必须区分：

- `resource_type`：mysql/redis/mongodb 等；
- `resource_role`：datasource/cache/message-broker/search/pagination-dialect 等；
- `confidence`：识别置信度；
- `match_reasons`：命中的字段或规则。

例如 PageHelper：

```json
{
  "resource_type": "mysql",
  "resource_role": "pagination-dialect",
  "confidence": 0.35,
  "match_reasons": ["helper-dialect=mysql"]
}
```

不能把它排在真实 `spring.datasource` 之前。

## 7. 搜索设计

### 7.1 结构化查询优先

查询：

```text
warehouse mysql
```

解析为：

```text
data_id/service 命中 warehouse
AND resource_type = mysql
```

查询：

```text
warehouse redis
```

解析为：

```text
data_id/service 命中 warehouse
AND resource_type = redis
```

### 7.2 排序优先级

```text
结构化字段精确命中
> data_id/service 前缀命中
> section 命中
> resource_type 高置信度命中
> 配置 key 命中
> 正文文本命中
> 向量语义命中
> 弱匹配
```

### 7.3 兼容旧查询

不含结构化意图的普通查询继续使用现有语义搜索；结构化查询才启用字段级 AND。旧的 `SearchHit` 字段保留，新字段使用 `#[serde(default)]` 兼容旧消费者。

## 8. CLI 输出

默认人类输出：

```text
[配置/MySQL] uvp-warehouse-saas:spring.datasource
  分析: 配置应用的 MySQL 数据源连接和连接池参数。
  来源: dt://nacos/test/{namespace_id}/DEFAULT_GROUP/uvp-warehouse-saas#section=spring.datasource

  正文:
    spring:
      datasource:
        url: jdbc:mysql://mysql-write:3306/warehouse
        username: root
        password: mysql123
        # 注释和原有空格保留
```

规则：

- 默认不显示 JSON；
- 外围信息紧凑；
- 正文保留原始换行、缩进、注释、空格和值；
- 不把正文压成一行；
- 不擅自隐藏密码（用户已明确要求保留）；
- 不截断命中的完整区块；
- `--json` 输出完整结构化字段；
- `content/raw_content` 与 Nacos 原文逐字符一致。

## 9. 与现有目录逻辑隔离

明确不修改：

```text
WalkDir / collect_project_files
FS VirtualFile 构造
本地 IncrementalStrategy
code_methods
doc_chunks 的本地目录统计
```

Nacos 配置必须带：

```text
source_type=nacos
```

本地文件必须带：

```text
source_type=filesystem
```

`dt://nacos/...` 不得作为本地路径参与目录索引。

## 10. 实施阶段

### 阶段 0：隔离与基线

- 创建隔离分支或备份工作树；
- 记录当前 release SHA256、Qdrant 集合数量、Memgraph 节点数量；
- 记录现有未提交改动，不覆盖；
- 记录已知测试失败；
- 不清库、不删除。

### 阶段 1：原文通道

- Nacos 原文保存为 `raw_content`；
- 保存 content_hash、版本和来源 URI；
- 增加逐字符保真测试；
- 不改变现有 embedding_text。

### 阶段 2：结构化资源索引

- 增加资源类型识别；
- 完善 MongoDB、Redis、MySQL、Kafka；
- 新增 service/dataId/section/resource_type/resource_role；
- 失败时不影响原文入库。

### 阶段 3：结构化搜索

- 增加查询意图解析；
- 实现字段级 AND；
- 结构化结果优先，向量结果兜底；
- 添加 match_reasons 和 confidence。

### 阶段 4：CLI 输出

- 默认人类紧凑模式；
- 原文完整输出；
- `--json` 保持兼容；
- 增加配置输出专项测试。

### 阶段 5：本地 LLM 语义分块

仅在确认本地模型、访问边界和 Schema 校验后启用：

- 注入 `Arc<dyn LlmService>`；
- LLM 输出只包含结构化边界；
- 原文由系统截取；
- LLM 失败进入 pending/retry/failed；
- 版本化 prompt/model/chunk。

### 阶段 6：闭环验收与 release

- 单元测试；
- 集成测试；
- 原文逐字符测试；
- MySQL/Redis/MongoDB/Kafka 测试；
- warehouse + mysql 不被 PageHelper 干扰；
- 本地目录、code/doc 回归；
- test Nacos 小范围验收；
- cargo fmt/check/test；
- release 构建、SHA256、部署和回滚验证。

## 11. 测试矩阵

### 原文保真

覆盖：

- LF/CRLF；
- Tab；
- 连续空行；
- 行尾空格；
- 中文注释；
- 引号；
- 密码；
- YAML、Properties、JSON、XML、自定义文本。

断言：

```text
截取后的 raw_content == 原始 Nacos content 的对应字符区间
```

### 结构化资源

- MySQL JDBC URL；
- Redis URI/host；
- MongoDB URI 与 Spring Mongo；
- Kafka bootstrap servers；
- PageHelper 只能成为弱匹配；
- 资源类型之间不得误识别。

### 搜索

- `warehouse mysql`：必须 dataId/service 与 MySQL 同时满足；
- `warehouse redis`：必须返回 Redis 配置；
- `warehouse mongodb`：必须返回 Mongo 配置；
- 无类型查询保持旧语义；
- 空结果返回空数组，不报错；
- JSON 合法且 stdout 纯净。

### 回归

- `dt build` 本地目录结果不变；
- code/doc/memory 搜索不受影响；
- 重复同步幂等；
- LLM 失败不删除旧版本；
- Embedding 超限只影响对应 chunk；
- release 可回滚。

## 12. 发布门槛

以下任何一项不满足，都不发布：

- `cargo fmt --check` 通过或既有差异已明确隔离；
- 新增单元/集成测试通过；
- 已知既有失败有独立归因；
- 原文保真测试通过；
- JSON 和默认 CLI 契约通过；
- Nacos 同步、Qdrant upsert、失败计数可核验；
- 本地目录回归通过；
- release SHA256 已记录；
- 有备份和回滚步骤；
- 未执行未经批准的清库/删除操作。

## 13. 回滚方案

- 代码回滚到当前 release SHA256；
- 新增字段使用兼容读取，旧 payload 仍可搜索；
- 新 collection/新索引使用版本后缀，避免覆盖旧 collection；
- 失败批次按 batch_id 回滚或标记无效；
- 不删除原有 NacosConfig、config_chunks；
- 恢复 CLI 后仍可读取旧字段。

## 14. 当前决策点

开始编码前需要确认：

1. 第一阶段是否先只实现原文通道、结构化资源识别和精确搜索；
2. 本地 LLM 是否已经可用，模型名称和服务地址是什么；
3. 若本地 LLM 未确认，是否保持 `chunk_strategy=rule`，仅预留 LLM-first 接口；
4. 是否允许 `raw_content` 写入 Qdrant payload，还是只存 Memgraph、Qdrant 保存引用；
5. 当前用户要求的密码保留范围是否同时适用于 CLI、JSON、Qdrant、Memgraph、备份和日志。

在上述决策明确前，方案只保存为文档，不进入 release 实施。
