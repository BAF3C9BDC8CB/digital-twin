# Phase 0 统一处理架构 — 实施结果与 G1-G6 缺口 (2026-08-07)

方案定稿: `/data/doc/设计方案/digital-twin-统一处理架构方案-2026-08-07-FINAL.md` (kimi-k3 终审, 有条件通过, 6 强制条件 F1-F6)。
本文件记录 Phase 0 实施+验证的**实际结果**, 供 Phase 1 继续时直接引用。

## 实施状态 (3 commits, 全部 done)

| commit | 内容 | 任务 |
|---|---|---|
| `7076d3b` | VirtualFile 抽象 + PipelineContext 扩展 + Processor::matches(&ctx) | t_77717140 (F1/F3) |
| `82c482a` | 增量 hash 策略 (远程源跳过 mtime 捷径直比 SHA256) + NacosVirtualFileSource | t_0e7423e4 (F2) |
| `e9d6e48` | prompt 词表 + F5 启动自检 + Store 写 config_chunks (purge 旧点) | t_b56f3858 (F4/F5/F6) |

回归 (t_00bd3334): **4/4 绿** — cargo test 694 通过/2 预存失败/0 新增; golden set 12/12;
无词表外 WARN; Memgraph 2769 = Qdrant 2769。
报告: `/data/doc/设计方案/phase0回归报告.md`

## 六项端到端验证结果 (t_2854cb9b) — 门禁未全绿

验证载体: `tests/phase0_verify_nacos.rs` (`#[ignore]`, 需 Nacos prod + Xinference + Memgraph + Qdrant 在线;
`cargo test --test phase0_verify_nacos -- --ignored --nocapture --test-threads=1`)。
报告: `/data/doc/设计方案/phase0验证.md` + `/data/doc/设计方案/phase0实体质量验证.md`

| # | 验证项 | 结果 | 数据 |
|---|---|---|---|
| (a) | 实体质量 LLM vs 正则 | ⚠️ 有条件 | 微观 precision 0.736 ≥ 0.732 过; 宏观 0.637 < 0.667; LLM 3/20 空输出 |
| (b) | 增量 改1条=1次LLM | ✅ | 186→0→1 条选中, 调用次数 1 (未变更=0) |
| (c) | 搜索兼容 | ✅ | Cypher 回退已覆盖 4 标签; 运行时 `dt search` 命中新实体 |
| (d) | 多源混合 fs+nacos | ✅ | 33/33 零冲突 (file:// vs dt://nacos/ 前缀隔离) |
| (e) | 纯 VirtualFile 端到端 | ✅ | dt:// 路径无磁盘文件 chunk→LLM 全链无 panic; 12 单测绿 |
| (f) | 性能基准 | ❌ | 单条 35.5-46.2s; 186 条全量估算 110-143 分钟 (预期 20-60) |

## G1-G6 缺口清单 (修完才能进 Phase 1)

| # | 级别 | 缺口 | 修复方向 |
|---|---|---|---|
| G1 | 阻塞验证 | `dt build --source nacos` CLI 是占位 (main.rs:1117-1121 自检后直接 return; `--env`/`--dry-run` 参数不存在) | 方案 Phase 1 落地 CLI 全家桶; 本次验证以组件层 harness 替代 |
| G2 | Bug | F5 自检误拒正确配置: `addr.parse::<SocketAddr>()` 不认 `localhost` 主机名 → "无效的 xinference 地址: localhost:9997" | build.rs:822-826 改用 `ToSocketAddrs` 解析 (一行级) |
| G3 | 接线缺口 | `select_prompt` (llm_client.rs:276-281) 无 nacos_config 分支 → Nacos 虚拟文件走 document_with_nlp, 产出全 Config 类型 | 增加 nacos 源 → nacos_config 路由 |
| G4 | 兼容性 | F4 词表 (NacosConfig/ConfigKey/ConfigSection/Database/Server) 与 EntityType 封闭枚举不兼容 → 一接线必 WARN 词表外 | prompt 词表与 model.rs 枚举同步 (3 处: 枚举 + as_str + Deserialize); 或 Nacos 独立 parse 路径 |
| G5 | 小问题 | NacosVirtualFileSource 对已带扩展名的 data_id 重复追加 .yaml → `common.yaml.yaml` | 扩展名去重 |
| G6 | 性能 | 单条 LLM 35-46s, 全量 110-143 分钟 | Phase 1: 块级并行 / max_tokens 裁剪 / 更新预期为 2 小时级 |

**结论**: (b)(c)(d)(e) 过, (a) 有条件过, (f) 未达预期; 门禁"无 WARN 词表外"因 G4 不满足;
F5 自检因 G2 无法在 CLI 层生效。按方案规则回到看板重新评审, 批准修复后再进 Phase 1。

## Phase 1 前置项

- 修 G2 (一行) → G3 (select_prompt 路由) → G4 (枚举同步) → G1 (CLI 接线) → G5 (扩展名)
- 实体质量优化: 3 条空输出配置 + properties precision
- 性能预期更新: 全量 110-143 分钟, 或并行化优化
- golden set 12 → 17 条 (新增 Q13-Q17 Nacos/Jenkins 专项)
