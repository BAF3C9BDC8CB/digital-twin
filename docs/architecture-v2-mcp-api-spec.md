# Digital Twin v2 MCP 接口规范

> ⚠️ **DEPRECATED**: 本文档已被 [V3 单 Crate 分层架构](./architecture-v3-single-crate-layered.md) 替代。
> V2 多 crate workspace 方案已废弃，实际实现采用单 crate 内部模块分层。
> 保留本文档仅供历史参考。

> 状态：当前实现 + v2 设计 | 日期：2026-07-09（已刷新：新增备份/归档/清理/指标/推理 MCP 工具）

本文档定义所有 MCP 工具的请求参数、返回内容及完整调用示例。

---

## 目录

**第一部分：当前已实现的 MCP 工具（22 个）**

1. [代码与知识搜索](#一代码与知识搜索)
2. [本地服务管理](#二本地服务管理)
3. [K8s 运维](#三k8s-运维)
4. [Jenkins CI/CD](#四jenkins-cicd)
5. [数据管道](#五数据管道)
6. [知识写入](#六知识写入)
7. [健康检查](#七健康检查)

**第二部分：v2 规划中的 MCP 工具（12 个）**

8. [v2 高层 MCP 接口](#八v2-高层-mcp-接口)

---

## 基础说明

### 通信机制

```
LLM / OpenCode
    │
    ▼  MCP Protocol (JSON-RPC)
┌───────────────────────┐
│  mcp-server.py        │  ← Python FastMCP server (gRPC client)
│  /home/luis/.local/   │
│  bin/digital-twin-mcp │
└───────────┬───────────┘
            │  gRPC :50051
            ▼
┌───────────────────────┐
│  dt daemon (Rust)     │  ← systemd socket activated
│  常驻后台 gRPC 服务     │
└───────────────────────┘
```

### 返回格式

所有工具统一返回 `[TextContent(type="text", text=text)]`，其中 `text` 为 gRPC 响应的结构化内容（不再解析 stdout/stderr）。

---

# 第一部分：当前已实现的 MCP 工具

---

## 一、代码与知识搜索

### 1. dt_search_kg — 知识图谱语义搜索

向量语义搜索 KG 节点，返回匹配节点及其 elementId。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `query` | string | ✅ | — | 自然语言搜索关键词 |
| `limit` | integer | ❌ | 10 | 返回数量上限 |

**请求示例：**

```json
{
  "query": "支付平台的数据库配置",
  "limit": 5
}
```

**底层命令：**

```bash
dt search-kg "支付平台的数据库配置" --limit 5
```

**返回示例：**

```text
[0.923] aflm-pay-mysql [Database]
  elementId: 4:05329859-cd55-4e7b-88d7-ae06c02df039:12345
  host: 10.0.1.50, port: 3306, db_type: MySQL, environment: prod

[0.887] nacos-pay-datasource [NacosConfig]
  elementId: 4:05329859-cd55-4e7b-88d7-ae06c02df039:67890
  data_id: pay-datasource.yml, group: DEFAULT, namespace: newoffen

[0.812] pay-service-config [Configuration]
  elementId: 4:05329859-cd55-4e7b-88d7-ae06c02df039:11111
  ...
```

**后续操作：** 拿到 elementId 后，用 `memgraph_read_cypher` 精确取完整属性。

---

### 2. dt_search_expand — 语义代码搜索

多查询变体合并去重，在 Qdrant 中搜索代码向量。推荐通过 `path` 限定搜索范围。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `query` | string | ✅ | — | 搜索关键词 |
| `path` | string | ❌ | — | 搜索范围路径（优先），搜索该路径下所有项目 |
| `name` | string | ❌ | — | 搜索范围项目名（path 未传时使用） |
| `limit` | integer | ❌ | 10 | 返回数量 |

> **注意：** `path` 优先级高于 `name`。通常传当前工作目录即可。

**请求示例：**

```json
{
  "query": "支付回调处理",
  "path": "/data/myProject/aflm-pay",
  "limit": 5
}
```

**底层命令：**

```bash
dt search "支付回调处理" --expand --json --limit 5 --path /data/myProject/aflm-pay
```

**返回示例：**

```text
[0.953] processPaymentCallback (src/main/java/com/aflm/pay/service/PayService.java:142)
    public Result processPaymentCallback(String orderId, PaymentResult result) { ...

[0.891] handleCallback (src/main/java/com/aflm/pay/controller/CallbackController.java:58)
    @PostMapping("/callback/{channel}") public Response handleCallback(@PathVariable String channel, ...

[0.845] notifyOrderStatus (src/main/java/com/aflm/pay/service/OrderService.java:301)
    public void notifyOrderStatus(String orderId, OrderStatus status) { ...
```

**返回字段说明：**
- `score`: 语义相似度 (0.0~1.0)
- `name`: 方法名
- `file_path`: 文件相对路径
- `start_line`: 起始行号
- `signature`: 方法签名（截断至 120 字符）

---

## 二、本地服务管理

### 3. svc_list — 列出所有本地微服务

**请求参数：** 无

**请求示例：**

```json
{}
```

**底层命令：**

```bash
svc list
```

**返回示例：**

```text
服务名称              状态      端口     PID      运行时间
─────────────────────────────────────────────────────────
aflm-pay              running   8080    12345    3d 12h 35m
aflm-admin            running   8081    12346    3d 12h 30m
aflm-gateway          stopped   —       —        —
aflm-scheduler        running   8082    12347    1d 5h 12m
```

---

### 4. svc_status — 查看微服务详细状态

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | string | ✅ | — | 服务名称 |

**请求示例：**

```json
{
  "name": "aflm-pay"
}
```

**底层命令：**

```bash
svc status aflm-pay
```

**返回示例：**

```text
服务: aflm-pay
状态: running
PID:  12345
端口: 8080
运行时间: 3d 12h 35m
内存使用: 512MB / 1024MB
CPU 使用: 15.3%
线程数: 42
配置文件: /data/myProject/aflm-pay/application.yml
日志文件: /data/myProject/aflm-pay/logs/app.log
```

---

### 5. svc_logs — 查看微服务运行日志

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | string | ✅ | — | 服务名称 |
| `lines` | integer | ❌ | 50 | 显示最近 N 行 |

**请求示例：**

```json
{
  "name": "aflm-pay",
  "lines": 20
}
```

**底层命令：**

```bash
svc logs aflm-pay --lines 20
```

**返回示例：**

```text
2026-07-09 14:32:15.123  INFO [http-nio-8080-exec-3] PayService: 处理支付请求 orderId=ORD20260709001
2026-07-09 14:32:15.145  INFO [http-nio-8080-exec-3] PayService: 渠道=allinpay, 金额=100.00
2026-07-09 14:32:15.234  INFO [http-nio-8080-exec-3] PayService: 支付成功, 流水号=TXN20260709001
2026-07-09 14:32:15.235  INFO [http-nio-8080-exec-3] PayController: POST /api/pay/order → 200 (112ms)
...
```

---

### 6. svc_start — 启动微服务（编译+启动）

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | string | ✅ | — | 服务名称 |

**请求示例：**

```json
{
  "name": "aflm-gateway"
}
```

**底层命令：**

```bash
svc start aflm-gateway
```

> 超时时间：300 秒（编译可能较慢）

**返回示例：**

```text
[编译] mvn clean package -DskipTests ... ✓ (45.2s)
[启动] java -jar target/aflm-gateway.jar ... ✓
[等待] 端口 8080 就绪 ... ✓ (8.3s)
服务 aflm-gateway 启动成功 (PID: 12350, 端口: 8080)
```

---

### 7. svc_stop — 停止微服务

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | string | ✅ | — | 服务名称 |

**请求示例：**

```json
{
  "name": "aflm-pay"
}
```

**底层命令：**

```bash
svc stop aflm-pay
```

**返回示例：**

```text
发送 SIGTERM → PID 12345 ... 等待退出 ... ✓ (3.2s)
服务 aflm-pay 已停止
```

---

### 8. svc_restart — 重启微服务

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | string | ✅ | — | 服务名称 |

**请求示例：**

```json
{
  "name": "aflm-pay"
}
```

**底层命令：**

```bash
svc restart aflm-pay
```

> 超时时间：300 秒

**返回示例：**

```text
[停止] 发送 SIGTERM → PID 12345 ... ✓ (3.2s)
[编译] mvn clean package -DskipTests ... ✓ (12.1s, 无变更)
[启动] java -jar target/aflm-pay.jar ... ✓
[等待] 端口 8080 就绪 ... ✓ (5.1s)
服务 aflm-pay 重启成功 (PID: 12355, 端口: 8080)
```

---

## 三、K8s 运维

### 9. kublog_status — 查看 K8s 资源状态

替代 kubectl，通过 Kuboard K8s API 代理查询。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 枚举 | 说明 |
|------|------|------|--------|------|------|
| `resource` | string | ✅ | — | `pods`, `deploy`, `svc` | 资源类型 |
| `namespace` | string | ❌ | `"default"` | — | 命名空间 |

**请求示例：**

```json
{
  "resource": "pods",
  "namespace": "newoffen"
}
```

**底层命令：**

```bash
kublog status pods --ns newoffen
```

**返回示例：**

```text
NAMESPACE   NAME                              READY   STATUS    RESTARTS   AGE   IP             NODE
newoffen    aflm-pay-7d8f9b6c-abcde           1/1     Running   0          3d    10.244.1.23    node-01
newoffen    aflm-pay-7d8f9b6c-fghij           1/1     Running   0          3d    10.244.2.45    node-02
newoffen    aflm-admin-5c8d7f-xyz12           1/1     Running   2          5d    10.244.1.67    node-01
newoffen    aflm-gateway-3f2e1d-mnopq         0/1     CrashLoop 5          1h    10.244.3.89    node-03
```

---

### 10. kublog_logs — 实时查看 Pod 日志

通过 Kuboard WebSocket 流式拉取，解决了网页日志断开问题。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `pod` | string | ✅ | — | Pod 名称 |
| `namespace` | string | ❌ | `"default"` | 命名空间 |
| `since` | string | ❌ | — | 回溯时间，如 `"30m"`, `"2h"` |
| `previous` | boolean | ❌ | `false` | 查看重启前的日志 |

**请求示例：**

```json
{
  "pod": "aflm-pay-7d8f9b6c-abcde",
  "namespace": "newoffen",
  "since": "30m"
}
```

**底层命令：**

```bash
kublog logs --ns newoffen --pod aflm-pay-7d8f9b6c-abcde --no-follow --since 30m
```

**返回示例：**

```text
2026-07-09 14:00:01.234 ERROR [pool-3-thread-1] PaymentService: 支付超时 orderId=ORD20260709002, channel=allinpay, timeout=30000ms
2026-07-09 14:00:01.456  WARN [pool-3-thread-1] PaymentService: 正在重试 (第1次/共3次) ...
2026-07-09 14:00:02.789  INFO [pool-3-thread-1] PaymentService: 重试成功, 流水号=TXN20260709002
2026-07-09 14:05:12.345  INFO [scheduling-1] OrderScheduler: 开始扫描超时订单 ...
2026-07-09 14:05:12.456  INFO [scheduling-1] OrderScheduler: 发现 3 笔超时订单, 执行取消
```

---

### 11. kublog_download — 下载 Pod 日志到本地

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `pod` | string | ✅ | — | Pod 名称 |
| `namespace` | string | ❌ | `"default"` | 命名空间 |
| `since` | string | ❌ | — | 回溯时间 |
| `output` | string | ❌ | — | 输出文件路径 |

**请求示例：**

```json
{
  "pod": "aflm-pay-7d8f9b6c-abcde",
  "namespace": "newoffen",
  "since": "24h",
  "output": "/tmp/aflm-pay-crash.log"
}
```

**底层命令：**

```bash
kublog download --ns newoffen aflm-pay-7d8f9b6c-abcde --since 24h -o /tmp/aflm-pay-crash.log
```

> 超时时间：300 秒

**返回示例：**

```text
已下载日志: aflm-pay-7d8f9b6c-abcde (newoffen)
时间范围: 2026-07-08 14:00 ~ 2026-07-09 14:00
大小: 1.2MB
保存至: /tmp/aflm-pay-crash.log
```

---

## 四、Jenkins CI/CD

### 12. jcli_list — 列出所有 Jenkins Job

**请求参数：** 无

**请求示例：**

```json
{}
```

**底层命令：**

```bash
jcli jobs
```

**返回示例：**

```text
Job 名称                                    类型           状态
─────────────────────────────────────────────────────────────
aflm-pay-deploy-test                        Pipeline       正常
aflm-pay-deploy-prod                        Pipeline       正常
aflm-admin-deploy-test                      Pipeline       正常
aflm-admin-deploy-prod                      Pipeline       禁用
aflm-gateway-deploy-test                    Pipeline       正常
aflm-scheduler-deploy-test                  Pipeline       正常
```

---

### 13. jcli_params — 查看 Job 参数定义

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `job` | string | ✅ | — | Job 名称 |

**请求示例：**

```json
{
  "job": "aflm-pay-deploy-test"
}
```

**底层命令：**

```bash
jcli params aflm-pay-deploy-test
```

**返回示例：**

```text
参数名              类型         默认值           说明
─────────────────────────────────────────────────────────
BRANCH              String       master          构建分支
VERSION             String       latest          版本号
PROFILE             Choice       test            Spring Profile (test/staging)
SKIP_TESTS          Boolean      true            跳过测试
NOTIFY_DINGTALK     Boolean      true            （钉钉通知）
```

---

### 14. jcli_history — 查看构建历史

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `job` | string | ✅ | — | Job 名称 |
| `limit` | integer | ❌ | 10 | 显示最近 N 条 |

**请求示例：**

```json
{
  "job": "aflm-pay-deploy-test",
  "limit": 5
}
```

**底层命令：**

```bash
jcli history aflm-pay-deploy-test -n 5
```

**返回示例：**

```text
#   状态       开始时间              耗时      分支        版本
─────────────────────────────────────────────────────────────────
#42  SUCCESS    2026-07-09 14:00:00   3m12s    master      v2.3.1
#41  SUCCESS    2026-07-09 10:30:00   2m58s    master      v2.3.0
#40  FAILURE    2026-07-09 09:00:00   1m23s    feature/xxx v2.3.0-rc1
#39  SUCCESS    2026-07-08 16:00:00   3m05s    master      v2.2.5
#38  SUCCESS    2026-07-08 14:00:00   2m47s    master      v2.2.4
```

---

### 15. jcli_build_log — 查看构建日志

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `job` | string | ✅ | — | Job 名称 |
| `build` | string | ❌ | (最新) | 构建编号 |

**请求示例：**

```json
{
  "job": "aflm-pay-deploy-test",
  "build": "42"
}
```

**底层命令：**

```bash
jcli log aflm-pay-deploy-test 42
```

**返回示例：**

```text
Started by user admin
Building on node 'builder-01'
[Pipeline] checkout
 > git rev-parse --resolve-git-dir /var/jenkins/...
[Pipeline] sh
 + mvn clean package -DskipTests
[INFO] BUILD SUCCESS
[Pipeline] sh
 + docker build -t aflm-pay:v2.3.1 .
[Pipeline] sh
 + kubectl set image deployment/aflm-pay aflm-pay=aflm-pay:v2.3.1 -n newoffen
deployment.apps/aflm-pay image updated
[Pipeline] }
Finished: SUCCESS
```

---

### 16. jcli_build — 触发 Jenkins 构建

⚠️ 仅当用户明确要求发布时使用。默认测试环境，明确说"正式/生产"才传 `production`。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 枚举 | 说明 |
|------|------|------|--------|------|------|
| `job` | string | ✅ | — | — | Job 名称 |
| `params` | string | ❌ | — | — | 构建参数，格式 `"KEY=VALUE, KEY2=VALUE2"` |
| `env` | string | ❌ | `"test"` | `test`, `production` | 环境 |

**请求示例 1 — 测试环境：**

```json
{
  "job": "aflm-pay-deploy-test",
  "params": "BRANCH=feature/payment-upgrade, SKIP_TESTS=false"
}
```

**请求示例 2 — 生产环境：**

```json
{
  "job": "aflm-pay-deploy-prod",
  "env": "production",
  "params": "VERSION=v2.3.1"
}
```

**底层命令：**

```bash
# 测试环境
jcli build aflm-pay-deploy-test -p BRANCH=feature/payment-upgrade -p SKIP_TESTS=false

# 生产环境
jcli build aflm-pay-deploy-prod -p VERSION=v2.3.1 --production
```

> 超时时间：600 秒

**返回示例：**

```text
触发构建: aflm-pay-deploy-test
构建编号: #43
参数: BRANCH=feature/payment-upgrade, SKIP_TESTS=false
构建 URL: https://jenkins.example.com/job/aflm-pay-deploy-test/43/
状态: 已加入队列, 等待执行...
```

---

## 五、数据管道

### 17. nacos_sync — 同步 Nacos 配置到知识图谱

修改 Nacos 配置后应触发此同步。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 枚举 | 说明 |
|------|------|------|--------|------|------|
| `env` | string | ❌ | `"all"` | `test`, `prod`, `all` | 目标环境 |

**请求示例：**

```json
{
  "env": "test"
}
```

**底层命令：**

```bash
dt nacos-sync --env test
```

> 超时时间：300 秒

**返回示例：**

```text
[Nacos] 连接 test 环境: http://nacos-test:8848
[命名空间] 发现 3 个命名空间
  → newoffen: 45 配置, 12 服务
  → newoffen-test: 38 配置, 8 服务
  → public: 5 配置, 2 服务
[配置] MERGE 88 个 NacosConfig 节点
[服务] MERGE 22 个 NacosService 节点
[实例] MERGE 45 个 NacosInstance 节点
[交叉链接] NacosService ↔ K8sService: 10 个匹配
完成: 3 个命名空间, 同步耗时 12.3s
```

---

### 18. dt_kg_sync — 同步 KG 节点到 Qdrant 向量库

KG 节点变更后应触发增量同步，使 `dt search-kg` 能语义搜索到最新节点。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `incremental` | boolean | ❌ | `false` | 仅同步未索引节点（`_kg_synced_at IS NULL`） |
| `labels` | string | ❌ | (全部业务标签) | 逗号分隔，如 `"Server,Database,NacosConfig"` |

**请求示例：**

```json
{
  "incremental": true
}
```

**底层命令：**

```bash
dt kg-sync --incremental
```

> 超时时间：300 秒

**返回示例：**

```text
[KG Sync] 增量模式
[查询] 找到 12 个未同步节点 (标签: Server, Database, K8sDeployment, Service, Knowledge, Experience, Concept, ...)
[嵌入] 12 个文本 → BGE-M3 (1024维) ... ✓ (2.1s)
[写入] Qdrant collection: kg_nodes, 12 points
[标记] SET n._kg_synced_at = datetime() ... ✓
完成: 12 个节点已同步到 Qdrant
```

---

### 19. dt_build — 增量构建代码索引

扫描项目，SHA1 比对文件，仅索引变更部分。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `path` | string 或 string[] | ✅ | — | 项目根路径或文件绝对路径 |
| `name` | string | ❌ | — | 项目名称（传目录时必填，传文件时自动解析） |

**请求示例 1 — 单文件更新（OpenCode Hook 自动触发）：**

```json
{
  "path": "/data/myProject/aflm-pay/src/main/java/com/aflm/pay/service/PayService.java"
}
```

**请求示例 2 — 项目全量构建：**

```json
{
  "path": "/data/myProject/aflm-pay",
  "name": "aflm-pay"
}
```

**请求示例 3 — 批量文件：**

```json
{
  "path": [
    "/data/myProject/aflm-pay/src/.../PayService.java",
    "/data/myProject/aflm-pay/src/.../OrderService.java"
  ]
}
```

**底层命令：**

```bash
# 单文件
dt build --file /data/myProject/aflm-pay/src/.../PayService.java

# 项目
dt build --path /data/myProject/aflm-pay --name aflm-pay
```

> 单文件超时 120 秒，项目超时 300 秒

**返回示例：**

```text
[build] aflm-pay @ /data/myProject/aflm-pay
[embed] BAAI/bge-m3 (running) (dim=1024)
[scan] found 312 files
[hash] 312 files (1.2s)
[compare] 308 unchanged, 4 changed/new, 0 deleted
[parse] 4 / 4 files (0.3s)
[methods] 23 total
[embed] 23 vectors (0.8s)
[write] 23 vectors → qdrant + memgraph (1.5s)
[class] 2 classes
[rels] created 12 CALLS relationships
[meta] Java project
[done] build complete (4.1s total)
```

---

## 六、知识写入

### 20. dt_memorize — 写入知识节点到 KG

用于架构决策、用户说"记住"等场景。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `type` | string | ✅ | — | 知识类型：`Decision`, `KnowledgeAdded`, `Environment`, `Dependencies` |
| `entity_id` | string | ✅ | — | 唯一标识 |
| `details` | string | ✅ | — | 详细内容，支持结构化字段 |
| `entity_type` | string | ❌ | — | 实体类型，如 `ArchitectureDecision`, `Concept` |
| `project` | string | ❌ | — | 所属项目 |

**details 结构化格式：**

```text
decision: <决策内容>; reason: <原因>; scope: <影响范围>
root_cause: <根因>; fix: <修复方案>
name: <名称>; description: <描述>
```

**请求示例 1 — 架构决策：**

```json
{
  "type": "Decision",
  "entity_id": "payment-platform-migration-20260709",
  "entity_type": "ArchitectureDecision",
  "project": "aflm",
  "details": "decision: 支付平台从通联切换到银盛; reason: 通联费率上涨且服务不稳定; scope: PayService, BusinessService, NacosCfg, DB"
}
```

**请求示例 2 — 用户口述记忆：**

```json
{
  "type": "KnowledgeAdded",
  "entity_id": "pay-ifcode-mapping",
  "entity_type": "Concept",
  "project": "aflm",
  "details": "name: ifCode映射; description: 支付渠道编码映射：通联=allinpay, 银盛=ysf, 微信=wechat, 支付宝=alipay"
}
```

**请求示例 3 — 执行结果采集：**

```json
{
  "type": "KnowledgeAdded",
  "entity_id": "exec-mysql-show-create-pay_order-20260709",
  "entity_type": "ExecutionResult",
  "project": "aflm",
  "details": "tool: mysql -e 'show create table pay_order'\nresult_summary: 订单表含12个字段(order_id, amount, channel, status, create_time...), 主键order_id, 索引idx_channel_status"
}
```

**底层命令：**

```bash
dt memorize --type Decision \
  --entity-id "payment-platform-migration-20260709" \
  --entity-type ArchitectureDecision \
  --project aflm \
  --details "decision: 支付平台从通联切换到银盛; reason: 通联费率上涨; scope: PayService, BusinessService"
```

**返回示例：**

```text
📝 已写入知识: Decision/payment-platform-migration-20260709 (ArchitectureDecision)
   Memgraph 节点 ID: knowledge-abc123def456
```

---

### 21. dt_event — 写入事件节点到 KG

用于部署、安装、配置变更、会话记录等事件溯源。

**请求参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `type` | string | ✅ | — | 事件类型：`Deploy`, `SoftwareInstalled`, `ConfigChange`, `Conversation` |
| `entity_id` | string | ✅ | — | 唯一标识 |
| `details` | string | ✅ | — | 详细内容 |
| `entity_type` | string | ❌ | — | 实体类型，如 `ServiceInstance`, `Software`, `NacosConfig`, `Session` |
| `project` | string | ❌ | — | 所属项目 |

**请求示例 1 — 部署事件：**

```json
{
  "type": "Deploy",
  "entity_id": "aflm-pay-deploy-prod",
  "entity_type": "ServiceInstance",
  "project": "aflm",
  "details": "branch: master, env: prod, version: v2.3.1, params: SKIP_TESTS=false"
}
```

**请求示例 2 — 软件安装事件：**

```json
{
  "type": "SoftwareInstalled",
  "entity_id": "redis-tools",
  "entity_type": "Software",
  "details": "version: 7.0.15, method: apt-get install redis-tools"
}
```

**请求示例 3 — 配置变更事件：**

```json
{
  "type": "ConfigChange",
  "entity_id": "pay-datasource.yml",
  "entity_type": "NacosConfig",
  "project": "aflm",
  "details": "修改项: spring.datasource.url, 旧值: jdbc:mysql://10.0.1.50:3306/pay, 新值: jdbc:mysql://10.0.2.50:3306/pay"
}
```

**请求示例 4 — 会话记录：**

```json
{
  "type": "Conversation",
  "entity_id": "2026-07-09",
  "entity_type": "Session",
  "project": "digital-twin",
  "details": "本次会话讨论了 v2 架构设计的六个世界模型、数据管道实现和 MCP 接口规范"
}
```

**底层命令：**

```bash
dt event --type Deploy \
  --entity-id "aflm-pay-deploy-prod" \
  --entity-type ServiceInstance \
  --project aflm \
  --details "branch: master, env: prod, version: v2.3.1"
```

**返回示例：**

```text
📝 已写入事件: Deploy/aflm-pay-deploy-prod
   event_id: evt-abc123def456
   已关联: Deployment:2026-07-09-prod → DEPLOYS→ServiceInstance:aflm-pay
```

---

## 七、健康检查

### 22. dt_health — 后端服务健康检查

**请求参数：** 无

**请求示例：**

```json
{}
```

**底层命令：**

```bash
dt health
```

**返回示例：**

```text
服务              状态      延迟      说明
─────────────────────────────────────────────────
Memgraph             ✓ 正常    12ms      bolt://localhost:7687
Qdrant            ✓ 正常    8ms       grpc://localhost:6334
Embed Server      ✓ 正常    5ms       grpc://localhost:50052 (BGE-M3)
KG Bridge         ✓ 正常    —         kg_nodes collection: 1,234 points
Fulltext Index    ✓ 正常    —         infra_search: 567 nodes indexed

全部服务正常 ✓
```

---

# 第二部分：v2 规划中的 MCP 工具

以下 12 个高层 MCP 是六世界模型设计中 v2 的目标接口（包含 4 个系统运维工具：dt_cleanup, dt_backup, dt_archive, dt_metrics），**部分尚未实现**。当前由 AI（Loom）通过编排上述 22 个底层工具来模拟。

---

### 23. dt_context — 聚合任务上下文

**定位：** 替代手动逐个查询六个世界，一次性返回任务所需的完整上下文。

**设计思路：**

```
输入任务描述
    ↓
Context Builder 解析意图 → 确定涉及哪些 World
    ↓
并行查询六世界（Retriever → Ranker → Dedup → Resolver → Summarize）
    ↓
返回聚合后的压缩上下文
```

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `task` | string | ✅ | 任务描述，如 `"支付平台从通联切换到银盛"` |
| `worlds` | string[] | ❌ | 限定查询的世界，默认全部：`["reality", "knowledge", "memory", "semantic", "runtime", "reasoning"]` |
| `max_tokens` | integer | ❌ | 上下文最大 token 数，默认 8000 |
| `thread_id` | string | ❌ | 关联的 Digital Thread ID |

**请求示例：**

```json
{
  "task": "支付平台从通联切换到银盛",
  "max_tokens": 6000,
  "thread_id": "thread-pay-migration-tonglian-to-yinsheng"
}
```

**返回示例（设计）：**

```json
{
  "thread": {
    "name": "支付平台迁移：通联 → 银盛",
    "status": "active",
    "history": "3 次会话, 12 个文件修改, 2 次部署"
  },
  "reality": {
    "services": ["aflm-pay (running, port 8080)"],
    "affected_code": [
      "PayService.java: ifCode, wayCode, merchantNo",
      "BusinessService.java: 新增银盛渠道逻辑"
    ],
    "configs": [
      "pay-datasource.yml: 数据库连接",
      "pay-channel.yml: ifCode=ysf, merchantNo=YINSHENG001"
    ],
    "database": {
      "pay_db": "MySQL @ 10.0.1.50:3306",
      "affected_tables": ["pay_order", "pay_channel_config"]
    }
  },
  "knowledge": {
    "patterns": ["支付平台迁移模式: 改 ifCode+wayCode+merchantNo+DB"],
    "playbooks": ["支付平台迁移 Playbook: 5 步骤"],
    "concepts": ["ifCode", "wayCode", "merchantNo", "channelExtra"]
  },
  "memory": {
    "similar_tasks": [
      "2025-12: 新增支付宝渠道 (参考价值: 高)",
      "2026-03: 微信支付升级 (参考价值: 中)"
    ],
    "pitfalls": [
      "⚠️ 别忘了同步修改 channelExtra",
      "⚠️ 回调地址需要在新平台后台配置"
    ]
  },
  "semantic": {
    "related_docs": [
      "docs/支付平台迁移指南.md (相似度: 0.92)",
      "docs/渠道接入规范.md (相似度: 0.85)"
    ]
  },
  "runtime": {
    "aflm-pay": {"status": "running", "cpu": "15%", "memory": "512MB/1024MB"},
    "k8s_pods": "2/2 Running"
  },
  "reasoning": {
    "previous_analysis": "上次分析结论: 需改 5 处 (conf: 0.9)",
    "decision_chain": "为什么选银盛: 费率低+API兼容好 → conf: 0.85"
  }
}
```

---

### 24. dt_plan — 生成执行计划

根据任务自动匹配 Playbook 生成执行计划。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `task` | string | ✅ | 任务描述 |
| `context` | string | ❌ | 已有的上下文（dt_context 返回值） |
| `thread_id` | string | ❌ | 关联的 Digital Thread ID |

**请求示例：**

```json
{
  "task": "支付平台从通联切换到银盛",
  "context": "{...dt_context 返回值...}"
}
```

**返回示例（设计）：**

```json
{
  "matched_playbook": {
    "name": "支付平台迁移 Playbook",
    "success_count": 3,
    "confidence": 0.92
  },
  "plan": [
    {
      "step": 1,
      "phase": "分析",
      "action": "查询当前支付渠道配置",
      "tool": "dt_search",
      "target": "ifCode, wayCode, merchantNo 配置项"
    },
    {
      "step": 2,
      "phase": "分析",
      "action": "排查所有引用这些配置的代码",
      "tool": "dt_dependency",
      "target": "PayService, BusinessService"
    },
    {
      "step": 3,
      "phase": "修改",
      "action": "修改渠道配置",
      "tool": "edit + nacos_sync",
      "target": "NacosConfig: pay-channel.yml",
      "detail": "allinpay → ysf, 新增银盛商户号"
    },
    {
      "step": 4,
      "phase": "修改",
      "action": "修改代码适配新渠道",
      "tool": "edit",
      "target": "PayService.java, BusinessService.java",
      "detail": "ifCode=ysf, wayCode=ysf, merchantNo=YINSHENG001"
    },
    {
      "step": 5,
      "phase": "验证",
      "action": "验证配置和代码一致性",
      "tool": "dt_verify",
      "target": "所有变更文件 + 数据库"
    },
    {
      "step": 6,
      "phase": "沉淀",
      "action": "记录迁移经验和决策",
      "tool": "dt_learn",
      "detail": "记录 pattern, pitfalls, decisions"
    }
  ],
  "estimated_impact": {
    "files": 5,
    "configs": 2,
    "database_changes": 1,
    "services_to_restart": ["aflm-pay"]
  }
}
```

---

### 25. dt_domain — 领域知识模型

返回某一业务领域的完整知识图谱子图。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `domain` | string | ✅ | 领域名，如 `"支付"`, `"部署"` |
| `depth` | integer | ❌ | 遍历深度，默认 2 |
| `include_code` | boolean | ❌ | 是否包含关联的代码实体，默认 true |

**请求示例：**

```json
{
  "domain": "支付",
  "depth": 2,
  "include_code": true
}
```

**返回示例（设计）：**

```json
{
  "domain": "支付",
  "concepts": [
    {
      "name": "ifCode",
      "definition": "支付渠道编码，用于路由到不同支付平台",
      "values": {"allinpay": "通联", "ysf": "银盛", "wechat": "微信", "alipay": "支付宝"},
      "used_in": ["PayService.java:142", "ChannelRouter.java:58"]
    },
    {
      "name": "wayCode",
      "definition": "支付方式编码",
      "related_to": ["ifCode", "merchantNo"]
    },
    {
      "name": "merchantNo",
      "definition": "商户号，每个支付渠道独立"
    }
  ],
  "services": ["aflm-pay"],
  "databases": ["pay_db"],
  "playbooks": ["支付平台迁移 Playbook", "新增支付渠道 Playbook"],
  "relationships": [
    {"from": "ifCode", "to": "wayCode", "type": "PAIRED_WITH"},
    {"from": "PayService", "to": "ifCode", "type": "IMPLEMENTED_BY"}
  ]
}
```

---

### 26. dt_history — 检索历史相似任务

沿 Memory World 时间线检索相似任务与修改记录。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `task` | string | ✅ | 当前任务描述 |
| `domain` | string | ❌ | 限定领域 |
| `days` | integer | ❌ | 回溯天数，默认 90 |
| `limit` | integer | ❌ | 返回数量，默认 5 |

**请求示例：**

```json
{
  "task": "支付平台切换",
  "domain": "支付",
  "days": 180,
  "limit": 3
}
```

**返回示例（设计）：**

```json
{
  "similar_tasks": [
    {
      "date": "2026-03-15",
      "task": "微信支付升级到 v3 API",
      "similarity": 0.88,
      "thread": "thread-wechat-v3-upgrade",
      "outcome": "成功",
      "key_learnings": ["v3 API 签名方式与 v2 完全不同", "需要更新商户证书"],
      "modified_files": ["PayService.java", "WechatPayConfig.java"],
      "pitfalls": ["证书路径不能写死，要放 Nacos"]
    },
    {
      "date": "2025-12-08",
      "task": "新增支付宝渠道",
      "similarity": 0.82,
      "thread": "thread-alipay-integration",
      "outcome": "成功",
      "key_learnings": ["渠道接入遵循统一接口模式", "ifCode+wayCode+merchantNo 三要素"],
      "modified_files": ["PayService.java", "AlipayChannel.java", "NacosCfg"],
      "pitfalls": []
    },
    {
      "date": "2025-08-22",
      "task": "支付平台数据库迁移",
      "similarity": 0.71,
      "thread": "thread-pay-db-migration",
      "outcome": "成功",
      "key_learnings": ["迁移前必须备份", "切换时关停服务 5 分钟"],
      "modified_files": ["datasource.yml", "DB migration scripts"],
      "pitfalls": ["连接池参数需要调大，否则迁移后超时"]
    }
  ]
}
```

---

### 27. dt_dependency — 调用链与依赖分析

返回实体间的调用链、依赖关系和影响范围。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `target` | string | ✅ | 目标实体：方法名、类名或服务名 |
| `direction` | string | ❌ | `"upstream"` (谁调用了它), `"downstream"` (它调用了谁), `"both"` |
| `depth` | integer | ❌ | 遍历深度，默认 2 |
| `type` | string | ❌ | `"code"`, `"config"`, `"service"`, `"all"` |

**请求示例：**

```json
{
  "target": "PayService",
  "direction": "both",
  "depth": 2,
  "type": "all"
}
```

**返回示例（设计）：**

```json
{
  "target": "PayService",
  "upstream": {
    "callers": [
      "PayController.processPayment()",
      "CallbackController.handleCallback()",
      "OrderScheduler.retryTimeoutOrders()"
    ],
    "services": ["aflm-gateway → aflm-pay (HTTP)"]
  },
  "downstream": {
    "callees": [
      "BusinessService.validateOrder()",
      "ChannelRouter.route() → AlipayChannel.pay() / WechatChannel.pay()",
      "OrderService.updateOrderStatus()"
    ],
    "databases": ["pay_db (MySQL)", "pay_cache (Redis)"],
    "configs": [
      "pay-datasource.yml: spring.datasource.url",
      "pay-channel.yml: payment.channels",
      "pay-timeout.yml: payment.timeout"
    ],
    "external": [
      "银盛支付 API: https://api.ysf.com/pay",
      "支付宝 API: https://openapi.alipay.com/gateway.do"
    ]
  },
  "impact_analysis": {
    "if_change_PayService": {
      "directly_affected": ["PayController", "CallbackController"],
      "config_dependencies": ["pay-channel.yml", "pay-timeout.yml"],
      "service_dependencies": ["aflm-gateway (需重启)"]
    }
  }
}
```

---

### 28. dt_verify — 修改后一致性验证

修改完成后验证受影响配置、数据库、接口的一致性。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `files` | string[] | ✅ | 变更的文件绝对路径列表 |
| `check_config` | boolean | ❌ | 检查 Nacos 配置一致性，默认 true |
| `check_db` | boolean | ❌ | 检查数据库 schema 一致性，默认 true |
| `check_api` | boolean | ❌ | 检查 API 签名一致性，默认 true |

**请求示例：**

```json
{
  "files": [
    "/data/myProject/aflm-pay/src/main/java/.../PayService.java",
    "/data/myProject/aflm-pay/src/main/java/.../BusinessService.java"
  ],
  "check_config": true,
  "check_db": true
}
```

**返回示例（设计）：**

```json
{
  "files_checked": 2,
  "checks": {
    "code_consistency": {
      "status": "✓",
      "detail": "2 个文件 AST 解析正常，方法签名无冲突"
    },
    "config_consistency": {
      "status": "⚠",
      "warnings": [
        "pay-channel.yml 中 ifCode 已改为 ysf，但 pay-timeout.yml 仍引用 allinpay 超时配置",
        "新增的 merchantNo=YINSHENG001 未在 pay-secret.yml 中配置对应密钥"
      ]
    },
    "database_consistency": {
      "status": "✓",
      "detail": "pay_channel_config 表已包含 ysf 渠道记录"
    },
    "api_consistency": {
      "status": "✓",
      "detail": "2 个 API 端点签名未变更"
    }
  },
  "overall": "⚠ 有 2 个警告, 建议修复后再部署",
  "suggestions": [
    "更新 pay-timeout.yml 中 allinpay 超时配置为 ysf",
    "在 pay-secret.yml 中新增 YINSHENG001 的密钥配置"
  ]
}
```

---

### 29. dt_learn — 任务完成后写回知识

与当前的 `dt_memorize` 类似，但为 v2 高层设计，语义更丰富。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `task` | string | ✅ | 任务名称，如 `"支付平台迁移：通联→银盛"` |
| `entities` | string[] | ✅ | 涉及的实体 ID 列表 |
| `pattern` | string | ❌ | 发现的模式，如 `"ifCode+wayCode+merchantNo+DB"` |
| `pitfalls` | string[] | ❌ | 踩坑记录 |
| `decisions` | object[] | ❌ | 决策记录 |
| `thread_id` | string | ❌ | 关联的 Digital Thread ID |
| `success` | boolean | ❌ | 是否成功 |
| `playbook_id` | string | ❌ | — | 关联的 Playbook ID（更新 success_count/failure_count） |

**请求示例：**

```json
{
  "task": "支付平台迁移：通联 → 银盛",
  "entities": ["PayService", "BusinessService", "pay-channel.yml", "pay_channel_config"],
  "pattern": "ifCode + wayCode + merchantNo + DB + channelExtra",
  "pitfalls": [
    "别忘了同步修改 channelExtra",
    "回调地址需要在银盛后台配置，否则支付成功但回调失败",
    "pay-timeout.yml 容易遗漏"
  ],
  "decisions": [
    {
      "context": "选择银盛作为新的支付平台",
      "choice": "银盛",
      "rationale": "费率低 0.1%，API 兼容性好，对接工作量小",
      "alternatives": ["支付宝直连", "微信直连"]
    }
  ],
  "thread_id": "thread-pay-migration-tonglian-to-yinsheng",
  "success": true,
  "playbook_id": "dt://playbook/aflm/pay-migration"
}
```

**返回示例（设计）：**

```json
{
  "written": {
    "knowledge": {
      "name": "支付平台迁移模式",
      "domain": "支付",
      "source": "ai_task"
    },
    "experience": [
      {"pitfall": "别忘了同步修改 channelExtra", "severity": "warning"},
      {"pitfall": "pay-timeout.yml 容易遗漏", "severity": "warning"}
    ],
    "playbook": {
      "name": "支付平台迁移 Playbook",
      "steps": 6,
      "based_on": "本次迁移经验 + 历史 2 次相似任务"
    },
    "decision": {
      "title": "支付平台选型：银盛",
      "rationale": "费率低 0.1%，API 兼容性好"
    },
    "thread": {
      "status": "completed",
      "updated_at": "2026-07-09 16:30:00"
    }
  },
  "summary": "📝 已沉淀 1 个知识模式, 1 个 Playbook, 2 条踩坑经验, 1 个决策记录"
}
```

---

### 30. dt_search — 语义代码搜索

保留底层语义搜索能力，但在 v2 中不再是第一步（由 dt_context 聚合）。

**请求参数（设计）：** 与当前 `dt_search_expand` 相同，增加 `world` 参数可跨世界搜索。

| 参数 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `query` | string | ✅ | 搜索关键词 |
| `world` | string | ❌ | `"code"`, `"knowledge"`, `"doc"`, `"all"` |
| `path` | string | ❌ | 搜索范围路径 |
| `limit` | integer | ❌ | 返回数量 |

---

### 31. dt_cleanup — 数据生命周期清理

按 TTL 策略预览/执行过期数据清理。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `dry_run` | boolean | ❌ | true | 预览模式，不实际删除 |
| `targets` | string[] | ❌ | all | 清理目标：`"memory"`, `"reasoning"`, `"snapshots"`, `"all"` |

**请求示例：**
```json
{
  "dry_run": true,
  "targets": ["memory", "reasoning"]
}
```

**返回示例（设计）：**
```json
{
  "dry_run": true,
  "results": {
    "memory_events": {
      "before_date": "2025-07-09",
      "count": 1234,
      "size_estimate": "45MB",
      "action": "archive"
    },
    "reasoning_stale": {
      "older_than_days": 30,
      "count": 87,
      "action": "delete"
    },
    "snapshots_old": {
      "count": 156,
      "action": "delete"
    }
  },
  "summary": "预览模式：将归档 1234 条 Event，删除 87 条 stale Reasoning，清理 156 条旧快照"
}
```

---

### 32. dt_backup — 备份与灾难恢复

分层备份 Memgraph + Qdrant + SQLite，支持指定日期恢复。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `action` | string | ✅ | — | `"backup"`, `"restore"`, `"list"`, `"verify"` |
| `date` | string | ❌ | — | 恢复/验证的目标日期，如 `"2026-07-09"` |

**请求示例：**
```json
{
  "action": "restore",
  "date": "2026-07-09"
}
```

**返回示例（设计）：**
```json
{
  "action": "backup",
  "timestamp": "2026-07-09T03:00:00Z",
  "targets": {
    "memgraph": {"size": "250MB", "format": "dump", "checksum": "sha256:abc123..."},
    "qdrant": {"collections": 12, "size": "1.2GB", "format": "snapshot"},
    "sqlite": {"size": "15MB", "format": "file_copy"}
  },
  "location": "/var/lib/dt/backups/2026-07-09/",
  "duration_seconds": 45.3
}
```

---

### 33. dt_archive — Memory 数据归档

将超过 TTL 的 Memory.Event 数据导出为压缩 JSON 归档，释放 Memgraph 存储。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `before` | string | ❌ | — | 归档此日期之前的 Event，默认 365 天前 |
| `dry_run` | boolean | ❌ | true | 预览模式 |
| `output_dir` | string | ❌ | `/var/lib/dt/archive/` | 归档输出目录 |

**请求示例：**
```json
{
  "before": "2026-01-01",
  "dry_run": false
}
```

**返回示例（设计）：**
```json
{
  "archive_file": "/var/lib/dt/archive/2025.json.gz",
  "events_archived": 5678,
  "events_remaining": 2345,
  "memgraph_space_freed": "120MB",
  "duration_seconds": 12.7
}
```

---

### 34. dt_metrics — 系统指标查询

通过 gRPC MetricsService 查询系统运行指标，不暴露 HTTP 端口。

**请求参数（设计）：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| `watch` | boolean | ❌ | false | 持续监听模式 |
| `interval` | integer | ❌ | 5 | 监听间隔（秒） |
| `filter` | string | ❌ | — | 过滤指标名，如 `"dt_build*"` |

**请求示例：**
```json
{
  "watch": true,
  "interval": 10,
  "filter": "dt_context*"
}
```

**返回示例（设计）：**
```json
{
  "timestamp": "2026-07-09T14:30:00Z",
  "gauges": {
    "dt_memgraph_connection_pool_size": 8,
    "dt_plugin_health_status{plugin=\"plugin_k8s\"}": 1,
    "dt_write_coordinator_active_locks": 2
  },
  "counters": {
    "dt_embed_requests_total{status=\"success\"}": 15234,
    "dt_qdrant_write_bytes_total": 1073741824
  },
  "histograms": {
    "dt_build_duration_seconds": {"p50": 4.2, "p99": 28.5, "count": 340},
    "dt_context_total_duration_seconds": {"p50": 1.2, "p99": 2.8, "count": 89}
  }
}
```

---

## 接口总览

### 当前已实现（22 个）

| # | 工具 | 类型 | 核心功能 |
|---|------|------|----------|
| 1 | `dt_search_kg` | 搜索 | KG 向量语义搜索 → elementId |
| 2 | `dt_search_expand` | 搜索 | 代码语义搜索 → 方法列表 |
| 3 | `svc_list` | 服务 | 列出所有本地微服务 |
| 4 | `svc_status` | 服务 | 查看微服务详细状态 |
| 5 | `svc_logs` | 服务 | 查看微服务运行日志 |
| 6 | `svc_start` | 服务 | 启动微服务（编译+启动） |
| 7 | `svc_stop` | 服务 | 停止微服务 |
| 8 | `svc_restart` | 服务 | 重启微服务 |
| 9 | `kublog_status` | K8s | 查看 Pod/Deploy/Service 状态 |
| 10 | `kublog_logs` | K8s | 实时查看 Pod 日志 |
| 11 | `kublog_download` | K8s | 下载 Pod 日志到本地 |
| 12 | `jcli_list` | CI/CD | 列出所有 Jenkins Job |
| 13 | `jcli_params` | CI/CD | 查看 Job 参数定义 |
| 14 | `jcli_history` | CI/CD | 查看构建历史 |
| 15 | `jcli_build_log` | CI/CD | 查看构建日志 |
| 16 | `jcli_build` | CI/CD | 触发 Jenkins 构建 |
| 17 | `nacos_sync` | 管道 | 同步 Nacos 配置到 KG |
| 18 | `dt_kg_sync` | 管道 | KG 节点同步到 Qdrant |
| 19 | `dt_build` | 管道 | 增量构建代码索引 |
| 20 | `dt_memorize` | 写入 | 写入知识节点到 KG |
| 21 | `dt_event` | 写入 | 写入事件节点到 KG |
| 22 | `dt_health` | 运维 | 后端服务健康检查 |

### v2 规划中（12 个）

| # | 工具 | 类型 | 核心功能 |
|---|------|------|----------|
| 23 | `dt_context` | 聚合 | 六世界聚合上下文（含 alerts 反馈） |
| 24 | `dt_plan` | 规划 | 匹配 Playbook 生成执行计划 |
| 25 | `dt_domain` | 查询 | 领域知识模型子图 |
| 26 | `dt_history` | 查询 | 历史相似任务检索（含归档数据） |
| 27 | `dt_dependency` | 分析 | 调用链 + 依赖 + 影响范围分析 |
| 28 | `dt_verify` | 验证 | 修改后的一致性验证 |
| 29 | `dt_learn` | 写入 | 任务完成后写回知识（含 Playbook 成功率反馈） |
| 30 | `dt_search` | 搜索 | 跨世界语义搜索 |
| 31 | `dt_cleanup` | 运维 | 按 TTL 策略清理过期数据 |
| 32 | `dt_backup` | 运维 | 分层备份与灾难恢复 |
| 33 | `dt_archive` | 运维 | Memory 超期数据归档 |
| 34 | `dt_metrics` | 监控 | gRPC 指标查询（无 HTTP 端口） |
