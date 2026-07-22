# `dt jc-sync` — Jenkins 同步命令设计

## 概述

新增 `dt jc-sync` 命令，将 Jenkins 的 View、Job 及构建历史同步到知识图谱（Memgraph），复用现有 `JenkinsApiClient` 和 `SyncSource` 架构。

## 命令接口

```bash
# 默认：同步所有环境的所有 Jenkins Job + 全部构建历史
dt jc-sync

# 指定环境
dt jc-sync --env test

# 指定 Job
dt jc-sync --job uvp-warehouse-center

# 组合
dt jc-sync --env prod --job uvp-order-center
```

## 知识图谱数据模型

### 节点类型

| 标签 | 说明 | 唯一约束 | 属性 |
|------|------|---------|------|
| `JenkinsView` | View（命名空间） | `view_id` | `view_id`, `name`, `url` |
| `JenkinsJob` | Job（服务） | `job_id` | `job_id`, `name`, `url`, `full_name`, `color`, `description`, `env` |
| `JenkinsBuild` | 构建 | `build_id` | `build_id`, `number`, `result`, `timestamp`, `duration`, `url`, `env` |

### 关系

```
(:JenkinsView)-[:CONTAINS]->(:JenkinsJob)
(:JenkinsJob)-[:HAS_BUILD]->(:JenkinsBuild)
(:JenkinsBuild)-[:NEXT_BUILD]->(:JenkinsBuild)   // 构建链（按编号顺序）
```

## 架构实现

### 分层结构（复用 nacos-sync 模式）

```
src/application/sync/jenkins/
├── mod.rs               — 模块导出
└── job_sync.rs          — JobSyncSource 实现 SyncSource trait

src/interfaces/cli/jenkins_sync.rs  — CLI handler
```

### 实现细节

#### 1. JenkinsSyncClient

复用现有的 `application::plugins::jenkins::client::JenkinsApiClient`，新增用于同步的结构化数据方法：

- `list_views()` → 获取所有 View 列表
- `list_jobs_for_view(view)` → 获取某个 View 下的 Job
- `get_job_detail(job)` → Job 详细信息 (url, description, color, fullName)
- `get_all_builds(job)` → 所有构建（不分页，全量拉取）
- `get_build_detail(job, number)` → 单次构建详情

#### 2. JobSyncSource

实现 `SyncSource` trait，`sync()` 流程：

1. 调用 `JenkinsApiClient.list_jobs()` 获取所有 Job（直接使用 `/api/json?tree=jobs[name,url,color,description,fullName]`）
2. 提取 Job 所属 View 信息（从 fullName 推断）
3. 逐个 Job：拉取全部构建历史 → MERGE 节点 + 关系
4. 使用 `MERGE` + `ON CREATE SET` / `ON MATCH SET` + content_hash 变更检测
5. 最后清理该环境下已不存在的孤立 Job/Build 节点

#### 3. 构建历史全量同步

- 通过 `/job/{name}/api/json?tree=builds[number,result,timestamp,duration,url]` 拉取全部构建
- Jenkins API 默认一次性返回所有 build，没有分页
- 对每个 Job 显示进度：`syncing uvp-warehouse-center... 142 builds`

#### 4. 与 `dt jcli build` 联动

`dt jcli build --job xxx` 触发构建后，自动执行增量同步：
- 调用 `JobSyncSource::sync_single_job(job_name)` 更新该 Job 的构建历史
- 通过 `dt event --type Deploy` 记录部署事件

## 文件变更清单

### 新增文件
| 文件 | 内容 |
|------|------|
| `src/application/sync/jenkins/mod.rs` | 模块导出 |
| `src/application/sync/jenkins/job_sync.rs` | `JobSyncSource` 实现 |
| `src/interfaces/cli/jenkins_sync.rs` | CLI handler |

### 修改文件
| 文件 | 变更 |
|------|------|
| `src/application/sync/mod.rs` | 添加 `pub mod jenkins;` |
| `src/infrastructure/memgraph/schema.rs` | 添加 3 个 Jenkins 约束 + 全文索引标签 |
| `src/interfaces/cli/mod.rs` | 添加 `pub mod jenkins_sync;` |
| `src/main.rs` | 添加 `JcSync` 命令变体 + match handler |
| `src/application/plugins/jenkins/client.rs` | 新增结构化 API 方法 |

## 约束与索引

```cypher
CREATE CONSTRAINT jenkins_view_id_unique IF NOT EXISTS FOR (n:JenkinsView) REQUIRE n.view_id IS UNIQUE
CREATE CONSTRAINT jenkins_job_id_unique IF NOT EXISTS FOR (n:JenkinsJob) REQUIRE n.job_id IS UNIQUE
CREATE CONSTRAINT jenkins_build_id_unique IF NOT EXISTS FOR (n:JenkinsBuild) REQUIRE n.build_id IS UNIQUE
```

在 `FULLTEXT_INDEX` 中添加 `JenkinsView`、`JenkinsJob`、`JenkinsBuild` 标签。
