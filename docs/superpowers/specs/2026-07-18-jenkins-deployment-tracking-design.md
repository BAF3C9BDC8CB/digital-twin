# Jenkins 部署追踪重构设计

> 日期: 2026-07-18
> 状态: 已批准 | 待实现

## 背景

KG 中存在 5 个与 Jenkins/部署相关的标签，其中部分设计存在重复和断层：

| 标签 | 数据量 | 用途 | 问题 |
|------|--------|------|------|
| `JenkinsView` | 11 | 视图分组 | 正常 |
| `JenkinsJob` | 208 | 流水线定义 | 正常 |
| `JenkinsBuild` | 1943 | 构建执行记录 | 正常 |
| `Deployment` | 0 | 发布记录 | **废弃**——与 JenkinsBuild 概念重叠且无关联 |
| `ServiceInstance` | 0 | 部署目标实例 | 保留并重新设计 |

## 设计目标

1. **废弃 `Deployment` 标签**——不再创建独立的 `(:Deployment)` 节点
2. **三节点联动**——发布时同时更新 JenkinsJob、JenkinsBuild、ServiceInstance
3. **保持事件时序**——`Day → Session → Modification` 仍记录操作时间线

## 数据模型

### 属性变更

```diff
 (:JenkinsJob)
   name, url, color, description, full_name, job_id
+  latest_deploy_env: STRING       # 最近部署环境
+  latest_deploy_version: STRING    # 最近部署版本
+  latest_deployed_at: STRING       # 最近部署时间 (ISO 8601)

 (:JenkinsBuild)
   number, result, duration, timestamp, url, build_id
+  deployed_env: STRING             # 部署环境 (如 "prod")
+  deployed_at: STRING              # 部署时间 (ISO 8601)
+  deployed_version: STRING         # 部署版本号

 (:ServiceInstance)                 # 重构：从 Deployment handler 中脱离
   instance_id: STRING              # "dt://service/{job}/instance/{env}"
   service_name: STRING
   env: STRING
   host: STRING (可选)
   port: INTEGER (可选)
   updated_at: STRING
```

### 关系变更

```diff
 (:JenkinsView)-[:CONTAINS]->(:JenkinsJob)           ← 已有
 (:JenkinsJob)-[:HAS_BUILD]->(:JenkinsBuild)          ← 已有
 (:JenkinsBuild)-[:NEXT_BUILD]->(:JenkinsBuild)       ← 已有

 (:Deployment)-[:DEPLOYS]->(:ServiceInstance)          ← 删除
 (:Deployment)-[:BELONGS_TO]->(:Project)               ← 删除

+ (:JenkinsJob)-[:LATEST_DEPLOY]->(:ServiceInstance)   ← 新增
+ (:JenkinsBuild)-[:DEPLOYED_TO {env, version, deployed_at}]->(:ServiceInstance)  ← 新增
```

### 完整查询示例

```cypher
-- 查询某个服务当前运行在哪
MATCH (j:JenkinsJob {name: "my-service"})-[:LATEST_DEPLOY]->(si:ServiceInstance)
RETURN si.service_name, si.env, si.host

-- 查询某次构建部署到了哪
MATCH (b:JenkinsBuild {number: 42})-[:DEPLOYED_TO]->(si:ServiceInstance)
RETURN b.number, si.service_name, si.env

-- 查询所有部署到生产的构建
MATCH (b:JenkinsBuild)
WHERE b.deployed_env = "prod"
RETURN b.number, b.deployed_at, b.deployed_version
ORDER BY b.timestamp DESC

-- 查询某个服务的完整部署历史
MATCH (j:JenkinsJob {name: "my-service"})-[:HAS_BUILD]->(b:JenkinsBuild)
WHERE b.deployed_env IS NOT NULL
RETURN b.number, b.deployed_env, b.deployed_at
ORDER BY b.timestamp DESC
```

## 触发流程

### 事件详情格式

`dt event --type Deployment` 的 `--details` 格式更新：

```
job: <job_name>; env: <env>; branch: <branch>;
version: <version>; build_number: <number>; status: <success|failure>
```

**必须包含 `build_number`**，handler 靠它找到对应的 `(:JenkinsBuild)` 节点。

### Handler 执行逻辑

```
AI 调用 jcli_build(job="xxx", params="...", env="prod")
  │
  ├─ 1. jcli_build 触发 Jenkins 构建
  ├─ 2. AI 调用 jcli_build_log 等待构建完成，拿到 build_number
  │
  └─ 3. 构建成功 → dt event --type Deployment --details "job: xxx; env: prod; build_number: 42; ..."
                        （不再创建 Deployment 节点，而是更新三节点）

       Handler 执行:
       a) 查找或兜底创建 (:JenkinsJob)
          MATCH (job:JenkinsJob {name: $job})
          // 未找到时兜底创建
          // 详见错误处理

       b) 查找或兜底创建 (:JenkinsBuild)
          MATCH (build:JenkinsBuild {build_id: "dt://jenkins/job/{$job}/build/{$build_number}"})
          SET build.deployed_env = $env,
              build.deployed_at = $now,
              build.deployed_version = $version

       c) 更新 Job 的最近部署信息
          SET job.latest_deploy_env = $env,
              job.latest_deploy_version = $version,
              job.latest_deployed_at = $now

       d) 创建/更新 ServiceInstance
          MERGE (si:ServiceInstance {instance_id: $instance_id})
          SET si.service_name = $job, si.env = $env, si.updated_at = $now

       e) 重建 LATEST_DEPLOY 关系（保持唯一）
          OPTIONAL MATCH (job)-[old:LATEST_DEPLOY]->()
          DELETE old
          MERGE (job)-[:LATEST_DEPLOY]->(si)

       f) 关联本次构建到部署目标
          MERGE (build)-[:DEPLOYED_TO {env: $env, version: $version, deployed_at: $now}]->(si)

       时序链: Day → Session → (Modification) ← 保留不变
```

## 改动清单

| # | 文件 | 改动内容 | 类型 |
|---|------|---------|------|
| 1 | `src/application/knowledge/memory/handlers/deployment.rs` | 重构 handler：不再 CREATE `(:Deployment)`，改为 MATCH+SET `JenkinsJob`/`JenkinsBuild` + MERGE `ServiceInstance` + 建关系 | 修改 |
| 2 | `src/application/sync/kg_bridge.rs` | 从 `BUSINESS_LABELS` 移除 `"Deployment"` | 修改 |
| 3 | `src/application/knowledge/memory/entities.rs` | 保留 `EventType::Deployment` 不变（兼容事件流），handler 逻辑改即可 | 不改 |
| 4 | `AGENTS.md` | 修复 `--type Deploy` → `--type Deployment`，更新指令描述 | 修改 |
| 5 | `docs/kg-empty-labels-analysis.md` | 更新 Deployment/ServiceInstance 状态 | 修改 |

## 不修改的

- `MemoryEvent` 结构体——不变
- `Day → Session → HAS_EVENT` 时序链——不变（事件仍记录，只是 handler 改写入目标）
- `EventType::Deployment` 枚举值——不变（兼容 `dt event --type Deployment` CLI）
- `Service` 标签（45条数据）——无关
- `K8sDeployment` 标签（111条数据）——无关

## 错误处理

- `MATCH (job:JenkinsJob)` 找不到时 → 创建 `(:JenkinsJob {name: $job, job_id: "dt://jenkins/job/$job", full_name: $job})` 兜底
- `MATCH (build:JenkinsBuild)` 找不到或 `build_number` 缺失时 → 创建 `(:JenkinsBuild {build_id: "dt://jenkins/job/$job/build/$timestamp", number: 0})` 兜底，记录 warning
- `build_number` 缺失 → handler 不报错，但创建的 JenkinsBuild 节点 `number: 0` 需要人工补全
- 任意步骤失败 → 不影响构建结果，日志 warning 级别

## 注意事项

- `LATEST_DEPLOY` 关系始终保持唯一：每次部署会先 DELETE 旧关系再 MERGE 新的
- `ServiceInstance` 按 `instance_id` 去重（`dt://service/{job}/instance/{env}`），重复部署同一环境不会创建重复节点
- 保留 `EventType::Deployment` 枚举值不变——`dt event --type Deployment` CLI 仍然可用，只是 handler 写入目标变了
