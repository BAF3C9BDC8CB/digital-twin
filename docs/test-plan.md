# Digital Twin V2 测试方案

> 日期：2026-07-10 | 测试项目：third-center (307 Java) + digital-twin-v2 (129 Rust)
> Memgraph: 4800+ 节点 | Qdrant: 2790+ vectors | MCP: 34 工具

---

## 一、测试环境

| 组件 | 地址 | 状态 |
|------|------|------|
| Memgraph | bolt://localhost:7687 | ✅ |
| Qdrant | grpc://localhost:6334 | ✅ |
| dt-embed | gRPC :50052 | ✅ BGE-M3 1024维 |
| Nacos | test: nacos.newoffen.net | ✅ |
| Kuboard K8s | 10.10.2.100:20080 | ✅ |

---

## 二、测试顺序

```
T1: dt health / dt daemon status    ← 基础
T2: dt build                        ← 核心管线
T3: dt search                       ← 代码搜索
T4: dt memorize + dt event          ← 知识写入
T5: dt context                      ← 六世界聚合
T6: dt nacos-sync                   ← 配置同步
T7: dt k8s-sync                     ← 基础设施
T8: dt kub pods / dt jcli list      ← 插件
```

---

## 三、测试结果

### T1: 健康检查 ✅
```
MCP: dt_health
  ✅ Memgraph   : healthy (1 ms)
  ✅ Qdrant  : healthy, v1.18.2
```

### T2: dt build ✅
```
third-center:    383 files, 2790 methods, 11s   → Memgraph + Qdrant vectors
digital-twin-v2: 307 files,  909 methods, 16s   → 自建成功
```

### T3: dt search ✅
```
查询 "MemgraphClient" → 找到 Class MemgraphClient
查询 "DaoBase"     → 找到 Interface DaoBase (Java接口修复后)
```

### T4: dt memorize / dt event ✅
```
dt memorize: "V2全链路测试" → Knowledge written
dt event: Modification → Event recorded
```

### T5: dt context ✅
```
任务 "支付平台从通联切换到银盛" → Reality: 23 items, ~190 tokens
6 世界管道完整运行
```

### T6: dt nacos-sync ✅
```
--env test → 350 NacosConfig + 42 Service + 880 ConfigKey 写入
```

### T7: dt k8s-sync ✅
```
111 K8sDeployment + 123 K8sService 写入
Node 403 (权限, 非代码问题)
```

### T8: 插件 ✅
```
dt kub pods --ns newoffen → 100+ pods (原生 HTTP API)
dt jcli list              → 150+ Jenkins jobs (原生 REST API)
```

---

## 四、MCP 工具列表 (34 个)

已通过 OpenCode MCP 协议注册，基于 `dt` CLI subprocess 调用。

### 搜索 (3)
- `dt_search_kg` — KG 向量语义搜索
- `dt_search_expand` — 代码语义搜索
- `dt_search` — 跨世界搜索

### 分析 (6)
- `dt_context` — 六世界聚合上下文
- `dt_plan` — Playbook 匹配
- `dt_domain` — 领域知识模型
- `dt_history` — 历史任务检索
- `dt_dependency` — 调用链分析
- `dt_verify` — 一致性验证

### 知识 (4)
- `dt_memorize` — 写入知识
- `dt_event` — 写入事件
- `dt_learn` — 沉淀知识
- `dt_thread` — Digital Thread

### 服务 (6)
- `svc_list/status/logs/start/stop/restart`

### K8s (3)
- `kublog_status/logs/download`

### Jenkins (5)
- `jcli_list/params/history/build_log/build`

### 管线/运维 (7)
- `dt_build`, `nacos_sync`, `dt_kg_sync`
- `dt_health`, `dt_cleanup`, `dt_backup`, `dt_metrics`
