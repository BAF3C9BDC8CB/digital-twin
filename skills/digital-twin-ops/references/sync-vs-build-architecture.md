# dt 构建的三大数据路径:文件管线 vs Nacos/Jenkins 同步

2026-08-07 用户提问"nacos构建 / jenkins构建从代码逻辑上如何处理,与其他类型文件处理有何不同"后整理。

> ⚠️ **2026-08-12 更新**: Jenkins 集成已整体移除（`dt jc-sync` / `dt jcli` / `application/plugins/jenkins` / `application/sync/jenkins` 全部删除，外部独立 `jcli` 二进制保留供 MCP jcli_* 工具使用）。本文第 3 节 Jenkins 架构为历史记录。
核心结论:**Nacos/Jenkins 不是"文件处理",它们走外部系统同步(SyncSource)体系,与文件构建是独立子系统。**

```
普通文件(dt build)   → 文件管线 pipeline(扫描→hash→LLM→向量化)
Nacos(dt nacos-sync) → SyncSource API 同步(拉取→MERGE→向量化到 config_chunks)
~~Jenkins(dt jc-sync / jcli build)~~ 已移除(2026-08-12)；Jenkins 数据摄入不再进 dt（外部 jcli 独立存在）
```

## 1. 普通文件构建(dt build)— pipeline 管线

- 入口 `src/application/build/pipeline.rs` + `updater.rs`
- 处理器链 4 个(`src/application/pipeline/processors/`):
  1. `tree_sitter.rs` — 代码 AST 解析
  2. `chunk.rs` — 文本分块
  3. `llm_client.rs` — **LLM 实体/关系/摘要抽取**(qwen3.5;重建慢的根源,2620 方法逐个分析)
  4. `store.rs` — 写 Memgraph + 向量化 Qdrant
- **增量机制**:`updater.rs` 用 `scanner::compute_file_hash()`(SHA1+mtime)对比,仅 hash 变化的文件重走管线;`--full` 全量
- 输出 Entity/Method/Class/Document 节点 + 向量到 `code_methods`/`doc_chunks`/`kg_nodes`
- 有 WriteCoordinator 写冲突保护

## 2. Nacos 同步(dt nacos-sync)— SyncSource

- 入口 `main.rs:1251` → `sync/service.rs` `NacosSyncService` → 注册 2 个 SyncSource:
- **ConfigSyncSource**(`sync/nacos/config_sync.rs`):
  - `list_namespaces()`(跳过 `old-*`/`public`/空 ns)→ 分页 `list_configs` 100/页 → `get_config_detail`
  - 内容 **SHA256 hash**,仅变化时 MERGE 更新(`NacosConfig` 节点 id=`dt://nacos/{ns}/{data_id}`)
  - 正则提取连接串(L41-58 JDBC/Redis/Kafka 正则)→ `Database` 节点
  - `extract_config_keys()` YAML/properties 启发式 → `ConfigKey` 节点;`classify_key()` 关键词分类(Database/Cache/MessageQueue/Server/Logging/Security/General)
  - 向量化到 **`config_chunks`**(不是 kg_nodes!)
- **ServiceSyncSource**(`sync/nacos/service_sync.rs`):`list_services()` → `NacosService` 节点 + `(:Service)-[:REGISTERED_IN]->(:NacosService)`

## 3. ~~Jenkins 同步(dt jc-sync / jcli build)— SyncSource~~（已移除 2026-08-12, 历史记录）

- 全量入口 `cli/jenkins_sync.rs`(`dt jc-sync`),核心 `sync/jenkins/job_sync.rs` `JobSyncSource`:
  - `list_views()` → `JenkinsView`(id=`dt://jenkins/view/{name}`);扁平 `/api/json?tree=jobs[...]` 拉全部作业保证覆盖
  - 每作业 `get_all_builds()` → `JenkinsBuild`(id=`dt://jenkins/job/{full_name}/build/{n}`)
  - 关系:`view-[:CONTAINS]->job`、build 间 `[:NEXT_BUILD]` 链
- **增量**:`cli/jcli.rs` L83-136 — `trigger_build()` 成功后自动对**该 job** 增量同步(`JobSyncSource::new(client, Some(job_name))` → `sync_job()`)并记部署事件
  - 注意:触发走 `application/plugins/jenkins/`(plugin 层),同步走 `application/sync/jenkins/`(sync 层),两层分离

## 4. 差异对照(核心)

| 维度 | 文件 `dt build` | Nacos `nacos-sync` | Jenkins `jc-sync` |
|---|---|---|---|
| 数据来源 | 本地文件扫描 | Nacos REST API | Jenkins REST API |
| 触发 | `dt build`(手动/增量) | `dt nacos-sync [test\|prod]` | `dt jc-sync` / `jcli build` 后自动 |
| 变更检测 | 文件 SHA1+mtime | 内容 SHA256 | 无(全量拉取) |
| 解析 | AST + LLM | 正则 + 启发式 | API 结构化字段 |
| LLM 参与 | ✅ | ❌ | ❌ |
| 向量化 | code_methods/doc_chunks/kg_nodes | **config_chunks** | ❌ 不向量化 |
| 输出节点 | Entity/Method/Document/Class | NacosConfig/ConfigKey/Database/NacosService | JenkinsView/JenkinsJob/JenkinsBuild |
| 抽象 | pipeline Processor trait | SyncSource trait | SyncSource trait |
| 写冲突 | WriteCoordinator | WriteCoordinator(冲突→skipped) | 同左 |

**一句话**:文件=深度 LLM 抽取(慢、准、带向量);Nacos/Jenkins=API 拉取+规则映射(快、结构确定;Nacos 带向量、Jenkins 不带)。两者靠 WriteCoordinator 防冲突,但文件有 hash 增量,外部同步靠"全量拉取 + MERGE 幂等"。

## 排查提示

- 搜不到 Nacos 配置内容 → 确认向量在 `config_chunks` 集合(world=config),不是 kg_nodes
- `jcli build` 后 KG 里作业没更新 → 检查 `sync_job` 增量是否因 `job` 参数缺失而跳过
- 想新增外部系统接入 → 实现 `SyncSource` trait(`sync/traits.rs`:`name()` + `sync(&graph)` → `SyncReport`),在 `sync/service.rs` 注册;报告字段含 nacos 专用(namespaces/configs/services/links)与 k8s 通用(items_*)两套
