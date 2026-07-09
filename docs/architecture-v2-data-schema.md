# Digital Twin v2 数据格式定义

> 状态：设计阶段 | 日期：2026-07-09

本文档定义六世界中每种数据内容的精确格式：节点标签、属性字段、类型、关系、存储位置。

---

## 一、Reality World（事实世界）

**存储：Neo4j**  
**特征：客观存在，可被自动发现**

### 1.1 Code Entity（代码实体）

#### Method（方法/函数）

```
标签: Method
属性:
  method_id         String   全局唯一 ID  dt://entity/{project}/class/{className}/method/{name}@{line}
  name              String   方法名
  signature         String   完整签名   "public String pay(String orderId)"
  params            String   参数列表   "String orderId, int amount"
  return_type       String   返回类型   "String"
  class_name        String   所属类名
  file_path         String   文件绝对路径
  package_or_module String   包名/模块名  "com.aflm.pay.service"
  language          String   语言       "java" | "python" | "ts" | "go"
  project           String   所属项目   "aflm-pay"
  start_line        Integer  起始行号
  end_line          Integer  结束行号
  calls             List<String>  内部调用的 method_id 列表
  comment           String   Javadoc/Docstring/注释摘要
关系:
  (:Class)-[:CONTAINS]->(:Method)
  (:Method)-[:CALLS]->(:Method)         // 方法调用
  (:Method)-[:BELONGS_TO]->(:Module)    // 所属模块
```

#### Class（类/接口/枚举）

```
标签: Class
属性:
  class_id     String   dt://entity/{project}/package/{package}/class/{name}
  name         String   类名
  kind         String   "Class" | "Interface" | "Enum" | "Struct"
  file_path    String   文件绝对路径
  package_or_module String  包名
  project      String   所属项目
  start_line   Integer
  end_line     Integer
  extends      List<String>  父类 class_id 列表
  implements   List<String>  接口 class_id 列表
关系:
  (:Class)-[:CONTAINS]->(:Method)
  (:Class)-[:EXTENDS]->(:Class)
  (:Class)-[:IMPLEMENTS]->(:Class)
```

#### Module / Package

```
标签: Module
属性:
  module_id    String   dt://entity/{project}/module/{name}
  name         String   模块/包名  "pay-service"
  project      String   所属项目
关系:
  (:Module)-[:CONTAINS]->(:Class)
  (:Module)-[:DEPENDS_ON]->(:Module)
```

### 1.2 Server（服务器/主机）

```
标签: Server
属性:
  server_id    String   唯一 ID
  name         String   主机名/标识
  hostname     String   IP 或域名
  port         Integer  SSH 端口
  auth_user    String   SSH 用户名
  auth_password String  SSH 密码（加密存储）
  credential_id String  凭证引用 ID
  service_type String   用途分类  "web" | "db" | "cache" | "mq"
  environment  String   环境      "prod" | "test" | "dev"
  cpu_cores    Integer  CPU 核数
  memory_gb    Float    内存 (GB)
  url          String   管理面板 URL
  description  String   描述
关系:
  (:Server)-[:RUNNING_ON]->(:OS)
  (:Server)-[:DEPLOYED_IN]->(:Environment)
```

### 1.3 Database

```
标签: Database
属性:
  database_id   String   唯一 ID
  name          String   数据库名 / 实例标识
  host          String   主机地址
  port          Integer  端口
  auth_user     String   用户名
  auth_password String   密码（加密存储）
  db_type       String   "MySQL" | "PostgreSQL" | "Redis" | "MongoDB" | "Kafka"
  engine        String   引擎版本  "InnoDB 8.0.33"
  version       String   数据库版本
  tables        List<String>  表名列表
  size_bytes    Integer  数据库大小
  environment   String   环境
  service_type  String   用途
  source_file   String   发现来源（Nacos 配置路径）
关系:
  (:Database)-[:RUNNING_ON]->(:Server)
  (:Database)-[:USED_BY]->(:Service)
  (:Table)-[:BELONGS_TO]->(:Database)
```

#### Table

```
标签: Table
属性:
  name      String   表名
  db        String   所属数据库名
  columns   List<String>  列名列表
  type      String   表类型  "BASE TABLE" | "VIEW"
  description String 注释
关系:
  (:Table)-[:BELONGS_TO]->(:Database)
  (:Table)-[:REFERENCED_BY]->(:Method)   // 代码中引用了此表
```

### 1.4 Config（配置）

#### NacosConfig

```
标签: NacosConfig
属性:
  config_id    String   唯一 ID
  data_id      String   Nacos dataId
  group        String   Nacos group
  namespace    String   Nacos namespace
  content      String   配置全文
  content_hash String   SHA256
  config_type  String   "properties" | "yaml" | "json" | "xml"
  updated_at   String   最后更新时间
关系:
  (:NacosConfig)-[:BELONGS_TO]->(:NacosGroup)
  (:NacosConfig)-[:CONFIGURES]->(:Service)
  (:NacosConfig)-[:CONTAINS]->(:ConfigKey)
```

#### ConfigKey（配置项）

```
标签: ConfigKey
属性:
  name       String   配置键名  "spring.datasource.url"
  value      String   配置值（脱敏后）
  namespace  String   所属命名空间
  purpose    String   用途描述
关系:
  (:ConfigKey)-[:BELONGS_TO]->(:NacosConfig)
```

### 1.5 API（接口/端点）

```
标签: Endpoint
属性:
  endpoint_id  String   唯一 ID
  method       String   HTTP 方法   "GET" | "POST" | "PUT" | "DELETE"
  path         String   路径       "/api/pay/order"
  controller   String   所属 Controller 类名
  description  String   接口说明（来自注解/Javadoc）
  params       String   参数列表
  return_type  String   返回类型
  project      String   所属项目
关系:
  (:Endpoint)-[:DEFINED_IN]->(:Class)
  (:Endpoint)-[:CALLS]->(:Method)        // 调用的后端方法
  (:Endpoint)-[:DEPENDS_ON]->(:NacosConfig)  // 依赖的配置
```

### 1.6 Document（文档）

```
标签: Document | Markdown | Note
属性:
  doc_id       String   唯一 ID
  name         String   文件名
  title        String   文档标题
  file_path    String   文件路径
  content      String   文本内容（Markdown 去格式后）
  summary      String   AI 生成摘要
  project      String   所属项目
  doc_type     String   "md" | "pdf" | "txt"
  tags         List<String>  标签
  size         Integer  文件大小
  modified     String   最后修改时间
关系:
  (:Document)-[:DESCRIBES]->(:Concept)        // 文档描述的概念
  (:Document)-[:BELONGS_TO]->(:Project)
```

### 1.7 Service（微服务）

```
标签: Service
属性:
  service_id   String   dt://entity/{env}/service/{name}
  name         String   服务名
  type         String   "spring-boot" | "nodejs" | "python"
  host         String   部署主机
  port         Integer  服务端口
  url          String   服务 URL
  framework    String   "Spring Boot 2.7"
  version      String   版本号
  config_path  String   配置文件路径
  log_path     String   日志文件路径
  status       String   "running" | "stopped"
关系:
  (:Service)-[:RUNNING_ON]->(:Server)
  (:Service)-[:DEPENDS_ON]->(:Database)
  (:Service)-[:DEPENDS_ON]->(:Service)       // 服务间调用
  (:Service)-[:REGISTERED_IN]->(:NacosService)
```

---

## 二、Knowledge World（知识世界）

**存储：Neo4j**  
**特征：概念、模式、经验，人类整理或 AI 沉淀**

### 2.1 Knowledge（知识条目）

```
标签: Knowledge
属性:
  knowledge_id  String   dt://knowledge/{project}/{domain}/{name}
  name          String   知识名称  "支付平台迁移模式"
  title         String   标题
  domain        String   领域      "支付" | "部署" | "配置"
  summary       String   一句话摘要
  content       String   详细内容（Markdown）
  definition    String   定义（概念类知识）
  source        String   来源      "ai_session" | "ai_task" | "document"
                        | "code_comment" | "user_dictation" | "execution_result"
  project       String   所属项目
  confidence    Float    置信度    0.0 ~ 1.0  (AI 生成的为低值，人工确认后为 1.0)
  verified_by   String   验证者    "human" | null
  created_at    DateTime
  updated_at    DateTime
关系:
  (:Knowledge)-[:RELATED_TO]->(:Knowledge)       // 相关知识
  (:Knowledge)-[:IMPLEMENTED_BY]->(:Method)      // 哪个代码实现了此知识
  (:Knowledge)-[:REFERENCES]->(:Document)        // 引用文档
  (:Knowledge)-[:BELONGS_TO]->(:Domain)          // 所属领域
```

### 2.2 Playbook（执行手册）

```
标签: Playbook
属性:
  playbook_id   String   dt://playbook/{project}/{name}
  name          String   "支付平台迁移"
  description   String   适用场景
  steps         List<Step>  执行步骤 [Step{order, action, tool, expected}]
  domain        String   领域
  project       String   所属项目
  success_count Integer  使用成功次数
  created_at    DateTime
关系:
  (:Playbook)-[:USES_KNOWLEDGE]->(:Knowledge)
  (:Playbook)-[:USES_TOOL]->(:Tool)
  (:Playbook)-[:RELATED_TO]->(:Playbook)
  (:Playbook)-[:BELONGS_TO]->(:Thread)
```

#### Step（嵌入子结构）

```json
{
  "order": 1,
  "action": "修改 ifCode",
  "tool": "edit",
  "target": "PayService.java",
  "expected": "ifCode 从 allinpay 改为 ysf",
  "pitfall": "别忘了同步改 channelExtra"
}
```

### 2.3 Experience（经验/踩坑）

```
标签: Experience
属性:
  experience_id String   dt://experience/{project}/{id}
  title         String   "Payment 模块 Redis 锁超时"
  summary       String   一句话教训
  content       String   详细经过
  domain        String   领域
  severity      String   "critical" | "warning" | "info"
  project       String
  created_at    DateTime
关系:
  (:Experience)-[:RELATED_TO]->(:Knowledge)
  (:Experience)-[:HAPPENED_IN]->(:Session)
  (:Experience)-[:REFERENCES]->(:Method)
```

### 2.4 Concept（概念/术语）

```
标签: Concept
属性:
  concept_id   String   dt://concept/{domain}/{name}
  name         String   概念名   "ifCode"
  definition   String   定义      "支付渠道编码"
  domain       String   所属领域  "支付"
  summary      String   详细说明
关系:
  (:Concept)-[:RELATED_TO]->(:Concept)
  (:Concept)-[:DESCRIBED_IN]->(:Document)
  (:Concept)-[:IMPLEMENTED_BY]->(:Method)
```

### 2.5 Domain（领域）

```
标签: Domain
属性:
  domain_id    String   dt://domain/{name}
  name         String   领域名  "支付" | "部署" | "配置"
  description  String   领域描述
关系:
  (:Domain)-[:CONTAINS]->(:Knowledge)
  (:Domain)-[:CONTAINS]->(:Concept)
```

---

## 三、Memory World（记忆世界）

**存储：Neo4j（只增不删，事件溯源）**  
**特征：时间线驱动，完整审计日志**

### 3.1 Day（天）

```
标签: Day
属性:
  day_id    String   "2026-07-09"
  date      String   日期
关系:
  (:Day)-[:HAS_SESSION]->(:Session)
```

### 3.2 Session（会话）

```
标签: Session
属性:
  session_id   String   "2026-07-09-001" 或 UUID
  summary      String   会话摘要
  key_decisions List<String>  关键决策
  thread_id    String   所属 Thread（可选）
  started_at   DateTime
  ended_at     DateTime
关系:
  (:Session)-[:HAS_EVENT]->(:Modification)
  (:Session)-[:HAS_EVENT]->(:Deployment)
  (:Session)-[:HAS_EVENT]->(:ConfigChange)
  (:Session)-[:HAS_EVENT]->(:BugFix)
  (:Session)-[:HAS_EVENT]->(:Decision)
  (:Session)-[:BELONGS_TO]->(:Thread)
```

### 3.3 Modification（代码修改）

```
标签: Modification
属性:
  mod_id       String   唯一 ID
  file         String   文件路径
  entity_type  String   修改的实体类型  "Method" | "Class" | "Config"
  entity_id    String   被修改实体的 ID
  change_type  String   "create" | "modify" | "delete"
  diff_summary String   变更摘要（AI 生成）
  reason       String   变更原因
  session_id   String   所属会话
  timestamp    DateTime
关系:
  (:Modification)-[:AFFECTS]->(:Method)     // 影响的实体（Neo4j 节点）
  (:Modification)-[:AFFECTS]->(:Class)
  (:Modification)-[:AFFECTS]->(:NacosConfig)
```

### 3.4 Deployment（部署事件）

```
标签: Deployment
属性:
  deploy_id    String   唯一 ID
  job          String   Jenkins Job 名称
  env          String   部署环境  "test" | "prod"
  branch       String   部署分支
  version      String   版本号
  params       String   构建参数（JSON）
  status       String   "success" | "failure"
  session_id   String   所属会话
  timestamp    DateTime
关系:
  (:Deployment)-[:DEPLOYS]->(:Service)
```

### 3.5 ConfigChange（配置变更）

```
标签: ConfigChange
属性:
  change_id    String   唯一 ID
  data_id      String   Nacos dataId
  key          String   变更的配置项
  old_value    String   旧值（脱敏）
  new_value    String   新值（脱敏）
  session_id   String   所属会话
  timestamp    DateTime
关系:
  (:ConfigChange)-[:AFFECTS]->(:NacosConfig)
```

### 3.6 BugFix（Bug 修复）

```
标签: BugFix
属性:
  fix_id       String   唯一 ID
  issue        String   Issue 编号 / 描述
  root_cause   String   根因分析
  solution     String   解决方案
  files_changed List<String>  变更文件
  session_id   String   所属会话
  timestamp    DateTime
关系:
  (:BugFix)-[:FIXES]->(:Method)
  (:BugFix)-[:RELATED_TO]->(:Experience)
```

### 3.7 Decision（架构决策记录）

```
标签: Decision
属性:
  decision_id   String   dt://decision/{project}/{id}
  title         String   决策标题  "为什么用 Redis 而不是本地缓存"
  context       String   背景和问题
  alternatives  List<String>  候选方案
  evidence      String   支撑证据
  choice        String   最终选择
  rationale     String   选择理由
  consequences  String   影响和后果
  confidence    Float    置信度
  verified      Boolean  是否已被验证
  session_id    String   所属会话
  timestamp     DateTime
关系:
  (:Decision)-[:BASED_ON]->(:Knowledge)
  (:Decision)-[:AFFECTS]->(:Method)
  (:Decision)-[:BELONGS_TO]->(:Thread)
```

---

## 四、Semantic World（语义世界）

**存储：Qdrant**  
**特征：向量化文本，相似度检索**

### 4.1 Code Snippet

```
Qdrant Collection: {project}_methods
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Neo4j 节点 ID（Method.method_id）
  name         String   方法名
  signature    String   方法签名
  class_name   String   所属类名
  file_path    String   文件路径
  language     String   语言
  start_line   Integer
  end_line     Integer
  project      String
  search_text  String   可搜索文本（签名 + 注释 + 方法体摘要）
```

### 4.2 Document Chunk

```
Qdrant Collection: {project}_semantic
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Neo4j 节点 ID（Document.doc_id）
  chunk_id     String   分块 ID   "{doc_id}#chunk{index}"
  text         String   分块文本
  doc_name     String   文档名
  doc_type     String   "md" | "pdf" | "txt"
  project      String
  start_offset Integer
  end_offset   Integer
```

### 4.3 Config Value

```
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Neo4j 节点 ID（ConfigKey 或 NacosConfig）
  key          String   配置键名
  value        String   配置值（脱敏后文本）
  namespace    String   Nacos namespace
  project      String
```

### 4.4 API Description

```
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Neo4j 节点 ID（Endpoint.endpoint_id）
  method       String   HTTP 方法
  path         String   API 路径
  description  String   接口描述
  controller   String   Controller 类名
  project      String
```

### 4.5 Experience

```
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Neo4j 节点 ID（Experience.experience_id）
  title        String   标题
  summary      String   摘要
  content      String   详细内容
  domain       String   领域
  project      String
```

### 4.6 Log Pattern（日志模式）

```
向量模型: BGE-M3 (1024 维)
Payload:
  pattern_id   String   唯一 ID
  template     String   日志模板  "Payment timeout for order {}: {}"
  service      String   来源服务
  severity     String   "ERROR" | "WARN" | "INFO"
  frequency    Integer  出现频次
```

---

## 五、Runtime World（实时世界）

**存储：不入库，实时查询**  
**特征：瞬时状态，随查随取**

### 5.1 Pod Status（Pod 状态）

```
来源: K8s Metrics API / Kuboard
格式:
{
  pod_name:    String,
  namespace:   String,
  phase:       String,        // "Running" | "Pending" | "Failed"
  node:        String,        // 所在节点
  ip:          String,        // Pod IP
  restarts:    Integer,
  cpu_usage:   String,        // "250m"
  memory_usage: String,       // "512Mi"
  containers:  [{name, image, ready, restarts}]
}
```

### 5.2 Service Metrics（服务指标）

```
来源: Spring Actuator / Prometheus（未来）
格式:
{
  service:     String,
  uptime:      String,        // "7d 12h 35m"
  heap_used:   String,        // "256MB / 512MB"
  thread_count: Integer,
  active_connections: Integer,
  request_count: Integer,
  error_rate:  Float          // 0.05 (5%)
}
```

### 5.3 Pod Logs（Pod 日志）

```
来源: Kuboard WebSocket
格式:
{
  pod:         String,
  namespace:   String,
  timestamp:   String,
  level:       String,        // "INFO" | "WARN" | "ERROR"
  message:     String,
  stacktrace:  String | null
}
```

### 5.4 Local Service Status（本地服务状态）

```
来源: svc_status MCP
格式:
{
  name:        String,        // 服务名
  status:      String,        // "running" | "stopped" | "error"
  pid:         Integer | null,
  port:        Integer,
  uptime:      String | null,
  memory_mb:   Float | null
}
```

---

## 六、Reasoning World（推理世界）

**存储：Neo4j（会话级）**  
**特征：AI 生成，验证后可升级为 Knowledge**

### 6.1 Observation（观察/模式发现）

```
标签: Observation
属性:
  observation_id String   dt://observation/{project}/{id}
  description    String   观察描述  "Module A 和 Module B 结构高度相似"
  evidence       String   支撑证据  "两者都有 payChannel、merchant、callback 三层"
  entities       List<String>  涉及的 entity_id 列表
  pattern        String   发现的模式
  confidence     Float    置信度
  session_id     String   发现会话
  timestamp      DateTime
关系:
  (:Observation)-[:ABOUT]->(:Method | :Class | :Service)
  (:Observation)-[:UPGRADES_TO]->(:Knowledge)    // 验证后升级
```

### 6.2 Analysis（分析过程）

```
标签: Analysis
属性:
  analysis_id    String   dt://analysis/{session}/{id}
  question       String   分析的问题  "切换支付平台影响哪些文件？"
  hypothesis     String   初始假设
  method         String   分析方法   "dependency_graph" | "semantic_search" | "git_diff"
  intermediate   List<Step>  中间步骤  [Step{action, result}]
  conclusion     String   分析结论
  confidence     Float    置信度
  total_cost_ms  Integer  总耗时 (ms)
  session_id     String
  timestamp      DateTime
关系:
  (:Analysis)-[:TRIGGERED_BY]->(:Session)
  (:Analysis)-[:EXAMINED]->(:Method | :Class)
  (:Analysis)-[:PRODUCED]->(:Observation)
  (:Analysis)-[:PRODUCED]->(:Decision)
```

### 6.3 Decision（推理决策）

```
（与 Memory World 中的 Decision 共用同一结构）
Memory.Decision = 已确认的决策（归档记录）
Reasoning.Decision = 推理中的决策（可能未确认）

生命周期:
  Reasoning.Decision (verified=false)
    → 执行确认
    → 升级: verified=true, 移动到 Memory/Knowledge
```

---

## 七、Digital Thread（数字主线）

**存储：Neo4j**  
**特征：跨六世界的横切层，串联业务演化链**

```
标签: Thread
属性:
  thread_id    String   dt://thread/{project}/{id}
  name         String   主线名称  "支付平台迁移：通联 → 银盛"
  description  String   主线描述
  status       String   "active" | "completed" | "archived"
  created_at   DateTime
  updated_at   DateTime
  project      String
关系:
  (:Thread)-[:HAS_REQUIREMENT] → (:Requirement)
  (:Thread)-[:HAS_SESSION]     → (:Session)
  (:Thread)-[:HAS_DECISION]    → (:Decision)
  (:Thread)-[:HAS_MODIFICATION]→ (:Modification)
  (:Thread)-[:HAS_DEPLOYMENT]  → (:Deployment)
  (:Thread)-[:HAS_KNOWLEDGE]   → (:Knowledge)
  (:Thread)-[:HAS_PLAYBOOK]    → (:Playbook)
  (:Thread)-[:RELATED_TO]      → (:Thread)
```

### Requirement（需求/任务）

```
标签: Requirement
属性:
  requirement_id  String   dt://req/{project}/{id}
  title           String   "支付平台从通联切换到银盛"
  description     String   需求描述
  priority        String   "P0" | "P1" | "P2"
  status          String   "todo" | "in_progress" | "done"
  created_at      DateTime
关系:
  (:Requirement)-[:DEPENDS_ON]->(:Requirement)
```

---

## 八、跨世界统一关系

所有实体可通过以下关系跨世界连接：

```
关系              起点             终点              语义
IMPLEMENTED_BY    Knowledge         Method/Class      知识被哪个代码实现
DESCRIBED_BY      Knowledge         Document          知识被哪个文档描述
REFERENCES        Knowledge         Document          知识引用哪个文档
AFFECTS           Modification      Method/Class/Cfg  修改影响了哪个实体
DEPLOYS           Deployment        Service           部署了哪个服务
FIXES             BugFix            Method            Bug 修复了哪个方法
BASED_ON          Decision          Knowledge         决策基于哪些知识
BELONGS_TO        *                 Thread/Project    属于哪个主线/项目
RELATED_TO        *                 *                 通用关联
```

---

## 九、存储总览

```
┌────────────┬──────────────────────────────────────────────────┐
│   存储层    │                    内容                          │
├────────────┼──────────────────────────────────────────────────┤
│ Neo4j      │ Reality (Code/Server/DB/Config/API/Doc/Service)  │
│            │ Knowledge (Knowledge/Playbook/Experience/Concept) │
│            │ Memory (Day/Session/Event/Decision/Fix)           │
│            │ Reasoning (Observation/Analysis/Decision)         │
│            │ Thread (Thread/Requirement)                       │
│            │ 全部关系                                           │
├────────────┼──────────────────────────────────────────────────┤
│ Qdrant     │ Semantic (Code/Doc/Config/API/Exp/Log vectors)    │
│            │ 通过 entity_id 反查 Neo4j                          │
├────────────┼──────────────────────────────────────────────────┤
│ 实时查询    │ Runtime (K8s Pod/Service Metrics/Logs/本地服务)    │
│            │ 不入库，Context Builder 动态拉取                    │
└────────────┴──────────────────────────────────────────────────┘
```
