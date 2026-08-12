# 团队 B 建议落地：检索质量增强四件套（2026-08-12, commit 45158fa）

背景：团队 B 审计发现 dt_search_kg 硬编码 knowledge world 导致 code 世界 0% 命中、跨项目噪音污染检索。修复后进一步落地四项增强（全部在 dt 源码，Rust）。

## 1. 低分降级提示（src/interfaces/cli/search_render.rs）

- 常量 `LOW_SCORE_THRESHOLD: f64 = 0.5`（注意：`SearchHit.score` 是 **f64**，fold 初值要用 `f64::NEG_INFINITY` + `f64::max`，用 f32 会 E0631 类型不匹配）
- `render_human` 中：全部命中低于阈值时输出 `⚠️ 结果可能不相关：最高分 X 低于阈值 0.5（常见原因：world 选错或跨项目噪音；可尝试 --world code + --project <项目名>）`
- 实测依据：跨项目污染结果普遍 <0.5，有效命中 >0.66

## 2. 索引对账巡检（src/interfaces/cli/cleanup.rs `run_health`）

- 读 Memgraph：`read_query("MATCH (m:Method) RETURN count(m) AS n", ...)` —— 注意 `read_query` 签名是 `(&str, HashMap)`，query 要 `&str` 不要 `.to_string()`
- 读 Qdrant：`vector.collection_info(CODE_METHODS).points_count`
- 解析 count：`v.pointer("/0/n").and_then(|x| x.as_i64())`（Memgraph 返回行数组）
- 相等 → ✅；不等 → ⚠ 索引漂移建议 --full 重建，且置 `all_healthy = false`
- 此功能会立即暴露残留项目/并行构建瞬态（见 residual-project-cleanup-and-index-reconcile.md）

## 3. 跨项目分组展示（search_render.rs `render_human`）

- 按 `h.project`（None → "（无项目）"）分桶，多项目时输出 `📦 命中项目分布: A / B（共 N 条）` + 每个项目 `── X ──` 分组
- 用 `HashMap<String, Vec<&SearchHit>>` + `order: Vec<String>` 保序

## 4. 知识层补全（dt learn）

- `dt build` 只索引 code 世界；knowledge 世界靠 `dt learn` 写入（Knowledge/Experience/Playbook 节点）
- 用法：`dt learn "im-center 消息发送链路" --project im-center --entities "A,B,C" --pattern "..." --pitfalls "..." --decisions "..." --success true`
- 写入后必须 `dt build --source knowledge` 同步节点到 Qdrant 向量（旧 `dt kg-sync` 已弃用，会打印 WARN），否则检索不到新节点
- 验证：`dt search "<关键词>" --world knowledge --project <项目> --json` 应命中 0.9+ 分的知识节点

## 查询词使用建议（复测数据）

- 中文查询带具体动词（"添加群成员" 而非 "群成员管理"）：中文口径 80% → 带动词/英文标识符 100%
- 功能名/标识符（accountImport、addGroupMember）用英文或中英混搭召回率最高
- 已写入 AGENTS.md 防再犯
