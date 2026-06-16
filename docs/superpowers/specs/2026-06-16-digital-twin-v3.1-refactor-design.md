# Digital Twin v3.1 激进重构 - 设计文档

> **日期** 2026-06-16 | **状态** 已批准

## 目标

修复 20 个已识别问题（8 高优 + 8 中优 + 4 低优），同时对 `engine-rust/src/` 进行模块拆分重构，消除代码重复，建立连接池，增量化 CALLS 构建，补全集成测试。

## 架构变更

```
engine-rust/src/
├── main.rs          # CLI 入口（不变）
├── config.rs        # 配置加载（移除硬编码默认密码）
├── models.rs        # 数据模型（不变）
├── scanner.rs       # 文件扫描（不变）
├── parser.rs        # 解析器（修复行号、class关联、非支持语言）
│
├── client/          # 新建：后端客户端层（连接池）
│   ├── mod.rs
│   ├── neo4j.rs     # 参数化查询，修复 Cypher 注入
│   ├── qdrant.rs    # 连接池复用
│   └── embed.rs     # 连接池复用
│
├── index/           # 新建：索引编排层
│   ├── mod.rs
│   ├── build.rs     # 增量构建
│   ├── full.rs      # 全量索引（流式，减少内存）
│   ├── update.rs    # 单文件更新（消除重复）
│   ├── remove.rs    # 实体删除
│   ├── callgraph.rs # CALLS 增量构建
│   └── convert.rs   # From trait 消除三处重复
│
├── search.rs        # 语义搜索（增加 call chain 展开）
├── health.rs        # 健康检查
├── validate.rs      # 验证
│
├── sync/            # 新建：外部同步
│   ├── mod.rs
│   ├── nacos.rs     # Nacos 同步
│   └── k8s.rs       # K8s 同步
│
├── event.rs         # 事件写入
├── knowledge.rs     # 知识写入
│
└── common/          # 新建：公共工具
    ├── mod.rs
    ├── hash.rs      # SHA1/SHA256 辅助
    └── error.rs     # 统一错误处理
```

## 20 项修复明细

### H1. 版本号统一 → 3.1.0
- `Cargo.toml:3`: `"3.0.0"` → `"3.1.0"`

### H2. search-web 缺失 lazy_consistency.py
- 新建 `services/search-web/lazy_consistency.py`
- `ConsistencyChecker` 类：`verify_and_repair()`, `discover_new_files()`, `_resolve_project_root()`

### H3. 凭证安全
- `config.rs`: 默认密码去除，空字符串强制配置
- `dt-sync`: base64 凭证 → 动态读取 config.yaml
- `search-web/app.py`: 移除硬编码 fallback

### H4. CALLS 增量构建
- `index/callgraph.rs`: `create_call_relationships_incremental(project, file_paths)`
- `build.rs`/`update.rs` 调用增量版；全量 index 仍用全局版

### H5. 行号修复
- `parser.rs`: `start_byte()` + 字符统计 → `start_position().row + 1`

### H6. 方法-类关联修复
- 删除 `|| m.file_path == c.file_path`
- parser 中填充 `class_name` 字段

### H7. MethodBlock 转换消除重复
- `index/convert.rs`: `From<&MethodBlock> for MethodNode` + `From<&MethodBlock> for Payload`

### H8. Cypher 注入修复
- `delete_all_methods`: 参数化查询

### M9. 连接池
- `client/` 模块：`OnceCell<reqwest::Client>` 全局单例

### M10. 错误处理
- `let _ =` → 至少 `eprintln!` 告警

### M11. 僵尸表 method_snapshots
- 删除 DDL 和空表

### M12. search-web 域搜索修复
- `code_method` → 动态获取集合名

### M13. 集成测试
- `engine-rust/tests/`: parser_test, convert_test, build_test, neo4j_test
- 临时资源测试后清理

### M14. 清理未用依赖和 import
- 删除 rayon, indicatif 如果未使用
- 清理 build.rs 中 AtomicUsize, Arc, ProgressBar, ProgressStyle

### M15. 非支持语言不错误解析
- 无匹配扩展名 → 返回空 ParsedFile

### M16. dt-sync 配置化
- 从 config.yaml 读取 Neo4j 连接信息

### L17. dt search 增加 call chain
- enrich_results 后查询 Neo4j callers/callees

### L18. 全量 index 流式处理
- parse → embed → write 交替，不一次收集全部 methods

### L19. Python 依赖锁定
- requirements.txt 精确版本

### L20. config.yaml.example 补全
- 增加 k8s, nacos, projects, document_dirs, watcher 示例

## 测试策略

| 测试文件 | 覆盖项 | 清理策略 |
|---------|--------|---------|
| `tests/parser_test.rs` | 行号(H5)、class_name(H6)、extract_calls、语言检测(H15) | tempfile 自动清理 |
| `tests/convert_test.rs` | MethodBlock→MethodNode/Payload(H7) | 纯内存 |
| `tests/build_test.rs` | 增量构建(H4)、SQLite、convert | 测试后 DROP collection |
| `tests/neo4j_test.rs` | 参数化查询(H8)、CALLS增量(H4) | 测试后 DELETE 节点 |

## 非目标

- 不改动 Neo4j / Qdrant / Embed Server 本身的配置或部署
- 不改动 setup.sh 的整体流程
- 不新增功能特性（仅修复 + 重构）
