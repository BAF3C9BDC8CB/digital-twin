# Digital Twin v2 数据格式定义

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：设计阶段 | 日期：2026-07-09（已刷新：新增 KnowledgeVersion 实体、备份/归档策略、chunking 参数、模型迁移命名）

本文档定义六世界中每种数据内容的精确格式：节点标签、属性字段、类型、关系、存储位置。

---

## 一、Reality World（事实世界）

**存储：Memgraph**  
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
  calls             List<String>  内部调用的方法名列表（通过正则提取，用于 CALLS 关系重建时按 name 模糊匹配。如需精确定位重载方法，应使用 file_path + start_line 组合定位）
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
  (:Server)-[:DEPLOYED_IN]->(:Environment)
  // (:Server)-[:RUNNING_ON]->(:OS) — OS 实体延后到 Phase 3+
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
```

### 1.7 Service（微服务 — 稳定标识）

```
标签: Service
属性:
  service_id   String   dt://service/{name}          ← 与环境无关
  name         String   服务名                         "aflm-pay"
  type         String   "spring-boot" | "nodejs" | "python"
  framework    String   框架版本                       "Spring Boot 2.7"
  description  String   服务说明
  project      String   所属项目
  config_path  String   配置文件路径
  log_path     String   日志文件路径
关系:
  (:Service)-[:DEPENDS_ON]->(:Database)              // 依赖的数据库
  (:Service)-[:DEPENDS_ON]->(:Service)               // 服务间调用
  (:Service)-[:REGISTERED_IN]->(:NacosService)       // 注册中心
  (:Service)-[:HAS_INSTANCE]->(:ServiceInstance)     // 各环境实例
```

#### ServiceInstance（服务实例 — 每环境一个）

```
标签: ServiceInstance
属性:
  instance_id   String   dt://service/{name}/instance/{env}
  service_id    String   反向引用 Service.service_id
  environment   String   "prod" | "test" | "dev" | "staging"
  host          String   部署主机 IP/域名
  port          Integer  服务端口
  url           String   完整访问 URL                  "http://10.0.1.50:8080"
  status        String   "running" | "stopped" | "unknown"
  version       String   当前部署版本                   "v2.3.1"
  replica_count Integer  副本数                        2

  // ─── Runtime 瞬态注入字段（每次请求实时拉取，不入 Memgraph） ───
  pods            Array(瞬态注入)   Pod 列表 (name, ip, phase, restarts, node, cpu, memory)
  cpu_usage       String (瞬态注入)  CPU 使用量 (聚合)       "250m"
  memory_usage    String (瞬态注入)  内存使用量 (聚合)        "512Mi"
  uptime          String (瞬态注入)  运行时长                 "7d 12h"
  heap_used       String (瞬态注入)  JVM Heap                "256MB / 512MB"
  thread_count    Integer(瞬态注入)  活跃线程数              42

  // 注意：pods[] 从 K8s API 实时拉取（GET /pods），不在 Memgraph 持久化
  // Pod 的历史问题（哪天哪个 Pod 崩溃了）→ Memory World 事件记录（Phase 2+）

关系:
  (:Service)-[:HAS_INSTANCE]->(:ServiceInstance)
  (:ServiceInstance)-[:DEPLOYED_AS]->(:K8sDeployment)     // 对应的 K8s Deployment
  (:ServiceInstance)-[:CONFIGURED_BY]->(:NacosConfig)  // 使用的配置（含环境差异）
```

**设计说明：** Service 是跨环境的稳定标识（service_id 不含 env），ServiceInstance 承载每个环境的具体部署信息。好处：
- 同一个服务名在不同环境有不同的 host/port/deployment，天然支持
- Runtime 实时指标（CPU/Mem/Uptime）作为缓存字段挂到 ServiceInstance 上，不被 Memgraph 持久化
  ⚠️ 注意：此处"缓存"是指 Context Builder 组装上下文时的瞬态注入字段，不持久化，TTL 由请求生命周期决定。
- Context Builder 组装上下文时，从 K8s API 实时拉取 Runtime 数据，注入到 ServiceInstance 的瞬态字段中

#### K8sDeployment（K8s Deployment — 稳定部署资源）

> ⚠️ 注意：此标签在 V2 中已从 Deployment 重命名为 K8sDeployment，以区别于 Memory World 中的 Deployment（部署事件记录）。

```
标签: K8sDeployment
属性:
  name          String   K8s Deployment 名称              "aflm-pay"
  namespace     String   K8s 命名空间                      "newoffen"
  image         String   容器镜像                          "aflm-pay:v2.3.1"
  replicas      Integer  期望副本数                        2
  available     Integer  可用副本数                        2
  strategy      String   更新策略                          "RollingUpdate"
  labels        Map      标签
  created_at    DateTime 创建时间
关系:
  (:ServiceInstance)-[:DEPLOYED_AS]->(:K8sDeployment)      // 对应的服务实例
```

### 1.8 K8sPod（已移除）

> **设计决策**：K8sPod **不属于 Reality World**。Pod 是 K8s 的运行时概念——每次部署、重启、调度都产生新 Pod。将 Pod 持久化到 Memgraph 会导致：脏数据（Pod 已终止但节点还在）、生命周期管理负担、无谓的写入开销。
>
> Pod 的全部信息（name、ip、phase、restarts、node、cpu、memory）属于 **Runtime World**，由 Context Builder 实时查询 K8s API 获取，注入到 `ServiceInstance.pods[]` 瞬态字段中。
>
> K8sDeployment 是 Reality 中唯一的 K8s 实体——它足够稳定，仅在部署时变化。K8sDeployment 不再有 `[:HAS_POD]` 关系（Pod 不入 Memgraph）。

---

## 二、Knowledge World（知识世界）

**存储：Memgraph**  
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
  (:Knowledge)-[:EVOLVED_FROM]->(:Knowledge)     // 版本演化链
```

#### KnowledgeVersion（知识版本记录）

```
标签: KnowledgeVersion
属性:
  version_id     String   dt://knowledge-version/{knowledge_id}/v{version}
  knowledge_id   String   所属知识节点 ID
  version        Integer  版本号（1, 2, 3...）
  diff           String   变更摘要  "新增 pitfall: pay-timeout.yml 容易遗漏"
  session_id     String   变更所属会话
  timestamp      DateTime 变更时间
关系:
  (:KnowledgeVersion)-[:RECORDS]->(:Knowledge)   // 记录的版本
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
  failure_count Integer  使用失败次数
  _needs_review Boolean  成功率 < 70% 时标记为 true
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

**存储：Memgraph（只增不删，事件溯源。TTL 365 天后归档 → /var/lib/dt/archive/）**  
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
  (:Session)-[:HAS_EVENT]->(:PodEvent)
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
  (:Modification)-[:BELONGS_TO]->(:Thread)    // 所属主线
  (:Modification)-[:AFFECTS]->(:Method)     // 影响的实体（Memgraph 节点）
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
  (:Deployment)-[:DEPLOYS]->(:ServiceInstance)   // 部署到哪个服务实例（含环境）
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

### 3.8 PodEvent（Pod 异常事件）

```
标签: PodEvent
属性:
  event_id      String   唯一 ID  "dt://podevent/{project}/{id}"
  pod_name      String   Pod 名称  "aflm-pay-7d8f9b6c-abcde"
  namespace     String   K8s 命名空间
  phase         String   Pod 状态  "CrashLoopBackOff" | "OOMKilled" | "Evicted"
  reason        String   原因描述  "OOMKilled: memory limit exceeded"
  message       String   K8s 事件消息
  node          String   所在节点
  container     String   出问题的容器名
  restart_count Integer  当时重启次数
  session_id    String   发现会话
  timestamp     DateTime 发生时间
关系:
  (:PodEvent)-[:AFFECTS]->(:ServiceInstance)   // 影响的哪个服务实例
  (:PodEvent)-[:RELATED_TO]->(:Deployment)     // 关联的部署事件（如有）
```

> **设计说明**：Pod 的全部运行时信息（name, ip, phase, restarts, cpu, memory）属于 Runtime World，不入 Memgraph。但当 Pod 出现异常时（如 CrashLoopBackOff、OOMKilled），通过 K8s 监控自动生成 PodEvent 事件节点，关联到对应 Session 或 Thread。这解决了"昨天 Pod 为什么 Crash"的历史追溯需求，同时不污染 Reality 数据。

---

## 四、Semantic World（语义世界）

**存储：Qdrant**  
**特征：向量化文本，相似度检索**

### 4.1 Code Snippet

```
Qdrant Collection: {project}_methods_{model_version}
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Memgraph 节点 ID（Method.method_id）
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
Qdrant Collection: {project}_semantic_{model_version}
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Memgraph 节点 ID（Document.doc_id）
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
  entity_id    String   对应 Memgraph 节点 ID（ConfigKey 或 NacosConfig）
  key          String   配置键名
  value        String   配置值（脱敏后文本）
  namespace    String   Nacos namespace
  project      String
```

### 4.4 API Description

```
向量模型: BGE-M3 (1024 维)
Payload:
  entity_id    String   对应 Memgraph 节点 ID（Endpoint.endpoint_id）
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
  entity_id    String   对应 Memgraph 节点 ID（Experience.experience_id）
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

**存储：不入 Memgraph，Context Builder 实时查询后注入 ServiceInstance 瞬态字段**  
**特征：瞬时状态，每次 dt_context 请求重新拉取**

### Reality vs Runtime 分界

```
Reality (Memgraph, k8s-sync 每小时)     Runtime (瞬态注入, 每次 dt_context 实时拉取)
────────────────────────────         ────────────────────────────────────────
(:K8sDeployment)                     ServiceInstance.pods[]:
  name: "aflm-pay"                     [{name, ip, phase, restarts, node,
  image: "aflm-pay:v2.3.1"              cpu, memory}, ...]
  replicas: 2
                                     ServiceInstance 瞬态注入字段:
(:ServiceInstance)                     cpu_usage, memory_usage
  host: "10.0.1.50"                    uptime, heap_used, thread_count
  port: 8080
  version: "v2.3.1"                  所有 Pod 信息不入 Memgraph
                                      所有 Metrics 不入 Memgraph
k8s-sync 无需管理 Pod 生命周期          每次查询都是最新数据
```

### 5.1 Pod List（Pod 列表 — 完整运行时信息）

```
来源: K8s API (GET /api/v1/namespaces/{ns}/pods)
注入目标: ServiceInstance.pods[]

格式:
[{
  name:           String,        // "aflm-pay-7d8f9b6c-abcde"
  ip:             String,        // "10.244.1.23"
  phase:          String,        // "Running" | "Pending" | "Failed"
  node:           String,        // "node-01"
  restarts:       Integer,       // 3
  cpu:            String,        // "250m"    (from Metrics API)
  memory:         String,        // "512Mi"   (from Metrics API)
}]
```

### 5.2 Service Metrics（服务指标）

```
来源: Spring Actuator / Prometheus / K8s Metrics API
注入目标: ServiceInstance 瞬态注入字段

Context Builder 查询流程:
  1. 从 Memgraph 获取 ServiceInstance（含 DEPLOYED_AS→K8sDeployment）
  2. 通过 K8sDeployment.name 查询 K8s API:
     GET /api/v1/namespaces/{ns}/pods?labelSelector=app={name}
     → 填充 ServiceInstance.pods[]
  3. 查询 K8s Metrics API:
     GET /apis/metrics.k8s.io/v1beta1/namespaces/{ns}/pods
     → 填充每个 pod 的 cpu, memory
  4. 查询 Actuator (未来):
     GET /actuator/metrics → heap_used, thread_count
  5. 查询本地服务状态 (开发环境):
     svc status → uptime, pid
   6. 注入到 ServiceInstance 瞬态字段
   7. 有效期: 当前请求（每次 dt_context 实时拉取，不入 Memgraph）
```

### 5.3 Pod Logs（Pod 日志）

```
来源: Kuboard WebSocket
格式:
{
  pod:             String,
  namespace:       String,
  timestamp:       String,
  level:           String,        // "INFO" | "WARN" | "ERROR"
  message:         String,
  stacktrace:      String | null
}
关联: 通过 pod name 匹配 ServiceInstance.pods[] 中的 Pod
```

### 5.4 Local Service Status（本地服务状态 — 开发环境专用）

```
来源: svc_status MCP
格式:
{
  name:            String,
  status:          String,        // "running" | "stopped" | "error"
  pid:             Integer | null,
  port:            Integer,
  uptime:          String | null,
  memory_mb:       Float | null
}
关联: 通过 name 匹配 Service（仅环境="dev" 的 ServiceInstance）
```

---

## 六、Reasoning World（推理世界）

**存储：Memgraph（会话级）**  
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

**存储：Memgraph**  
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

> ⚠️ **注意**：V2 已将 Reality World 的 K8s 实体重命名为 **K8sDeployment**（属性：name/image/replicas），以区别于 Memory World 中的 **Deployment**（部署事件记录，属性：deploy_id/job/env/version）。两者是完全不同的实体类型，`MATCH (d:Deployment)` 仅返回部署事件，`MATCH (d:K8sDeployment)` 仅返回 K8s 资源。关系区分：`[:DEPLOYED_AS]→K8sDeployment` vs `[:DEPLOYS]→ServiceInstance`。

所有实体可通过以下关系跨世界连接：

```
关系              起点             终点              语义
IMPLEMENTED_BY    Knowledge         Method/Class      知识被哪个代码实现
DESCRIBED_BY      Knowledge         Document          知识被哪个文档描述
REFERENCES        Knowledge         Document          知识引用哪个文档
AFFECTS           Modification      Method/Class/Cfg  修改影响了哪个实体
DEPLOYS           Deployment        ServiceInstance   部署了哪个服务实例
FIXES             BugFix            Method            Bug 修复了哪个方法
BASED_ON          Decision          Knowledge         决策基于哪些知识
BELONGS_TO        *                 Thread             属于哪个主线/项目
RELATED_TO        *                 *                 通用关联
HAS_INSTANCE      Service           ServiceInstance   服务的环境实例
DEPLOYED_AS       ServiceInstance   K8sDeployment     实例对应的 K8s Deployment
CONFIGURED_BY     ServiceInstance   NacosConfig       实例使用的配置（含环境差异）
// --- Digital Thread 主线关系 ---
HAS_REQUIREMENT   Thread            Requirement       主线关联的需求
HAS_SESSION       Thread            Session           主线关联的会话
HAS_DECISION      Thread            Decision          主线关联的决策
HAS_MODIFICATION  Thread            Modification      主线关联的代码变更
HAS_DEPLOYMENT    Thread            Deployment        主线关联的部署记录
HAS_KNOWLEDGE     Thread            Knowledge         主线关联的知识
HAS_PLAYBOOK      Thread            Playbook          主线关联的执行手册
```

---

## 九、存储总览

```
┌────────────┬──────────────────────────────────────────────────┐
│   存储层    │                    内容                          │
├────────────┼──────────────────────────────────────────────────┤
│ Memgraph      │ Reality: Code(Method/Class/Module) +             │
│            │   Server + Database + Table +                    │
│            │   Config(NacosConfig/ConfigKey) +                │
│            │   API(Endpoint) + Document +                     │
│            │   Service + ServiceInstance + K8sDeployment          │
│            │   (⚠️ K8sPod 已移除 — 全部属于 Runtime,           │
│            │    K8sDeployment 无 HAS_POD 关系)                     │
│            │ Knowledge: Knowledge + Playbook + Experience     │
│            │   + Concept + Domain                             │
│            │ Memory: Day + Session + Modification +           │
│            │   Deployment + ConfigChange + BugFix + Decision   │
│            │ Reasoning: Observation + Analysis + Decision     │
│            │ Thread: Thread + Requirement                     │
├────────────┼──────────────────────────────────────────────────┤
│ Qdrant     │ Semantic: Code/Doc/Config/API/Exp/Log vectors    │
│            │ 通过 entity_id 反查 Memgraph                          │
├────────────┼──────────────────────────────────────────────────┤
│ 文件系统    │ Backup: Memgraph dump + Qdrant snapshot + SQLite cp  │
│            │ 位置: /var/lib/dt/backups/{date}/                  │
│            │ Archive: Memory.Event JSON export (.json.gz)        │
│            │ 位置: /var/lib/dt/archive/{date_range}.json.gz     │
├────────────┼──────────────────────────────────────────────────┤
│ 运行时数据  │ Runtime (瞬态注入): ServiceInstance.pods[]         │
│            │ (name, ip, phase, restarts, node)                 │
│            │ + cpu_usage, memory_usage, uptime,                │
│            │ heap_used, thread_count                           │
│            │ Context Builder 实时查询 K8s API / Actuator       │
│            │ 有效期为当前请求，不入 Memgraph                        │
└────────────┴──────────────────────────────────────────────────┘
```

---

## 十、扩展指南：如何新增实体/关系/属性

### 扩展模式总览

| 你要做什么 | 改动文件 | 改动量 | 示例 |
|-----------|---------|--------|------|
| 实体新增属性 | `dt-common/src/types.rs` | 1 行 field | Service 加 `team: String` |
| 新增数据保留规则 | `dt-storage/src/memgraph/schema.rs` TTL 表 | ~5 行 YAML | Event 365 天 TTL |
| 新增子实体 | types.rs + repo trait + repo impl | ~40 行 | Service → +ServiceInstance |
| 新增独立实体 | types.rs + repo trait + repo impl + schema init | ~60 行 | 新增 Environment 实体 |
| 新增关系 | repo trait + repo impl | ~15 行 Cypher | RUNS_AS: Instance→Pod |
| Runtime 瞬态字段 | types.rs (ServiceInstance 瞬态字段区) | ~5 行 field | 新增 `disk_usage` |
| 数据源填充新字段 | 对应 sync/pipeline 文件 | 改采集逻辑 | nacos-sync 填 instance host |
| 新增数据源 | SyncSource trait 实现 | ~150 行 | Apollo 配置中心同步 |

### 原则

1. **Memgraph schemaless** — 加属性不需要 migration，加标签不需要 schema change（但需在 `schema init` 中加约束索引）
2. **trait 先行** — 先在 `dt-common/src/traits.rs` 中定义接口，再在 `dt-storage` 中实现
3. **ServiceInstance 是扩展枢纽** — 任何与环境相关的字段都挂在 ServiceInstance，不污染 Service
4. **瞬态字段不入 Memgraph** — Runtime 数据标记为 `(瞬态注入)`，由 Context Builder 实时注入
5. **关系命名规范** — 全大写动词：`HAS_INSTANCE`, `DEPLOYED_AS`, `CONFIGURED_BY`

### 完整示例：新增 Environment 实体

假设未来需要将环境（prod/test/dev）作为独立实体管理：

```
步骤 1: types.rs
  pub struct Environment {
      pub env_id: String,     // dt://env/prod
      pub name: String,        // "生产环境"
      pub code: String,        // "prod"
      pub description: String,
  }

步骤 2: traits.rs
  async fn upsert_environment(&self, env: &Environment) -> Result<()>;

步骤 3: memgraph/repo.rs
  MERGE (e:Environment {env_id: $id})
  SET e.name = $name, e.code = $code, e.description = $desc

步骤 4: schema/mod.rs
  CREATE CONSTRAINT env_id_unique IF NOT EXISTS
  FOR (e:Environment) REQUIRE e.env_id IS UNIQUE

步骤 5: 迁移现有关系
  MATCH (si:ServiceInstance)
  MERGE (e:Environment {env_id: "dt://env/" + si.environment})
  MERGE (si)-[:DEPLOYED_IN]->(e)
```

### 不改的部分（自动生效）

以下组件不需要任何修改即可感知新增的实体和关系：
- **gRPC proto** — 数据在内部流动，不暴露新接口
- **Context Builder** — `MATCH (n) RETURN n` 自动发现新节点类型
- **插件系统** — 不涉及
- **日志系统** — `tracing::instrument` 自动覆盖
- **DI 装配** — trait 注入，编译期自动解析
