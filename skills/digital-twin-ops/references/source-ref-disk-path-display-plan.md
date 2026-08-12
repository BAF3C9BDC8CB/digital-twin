# source_ref → 磁盘全路径显示方案(2026-08-11 可行性调研已定,实施时照此执行)

用户提案:搜索结果来源字段显示时,把 `dt://doc/{项目名}/{相对路径}` 解析为磁盘全路径(config.yaml 项目别名 → base+子目录映射)。以下为已核实的代码事实 + 推荐实施计划(**展示层方案**,不改 JSON/MCP/持久化数据)。

## 已核实代码事实(行号基于 commit 2ac4b43)

- `SearchHit`(src/application/context/search_mcp.rs:241-301)**无 project 字段**;`#[serde(default)]` 的 Option 字段是该 struct 的既有惯例
- **code 世界**:`file_path`=相对路径(payload 直取)、`source_ref=None`(search_mcp.rs:151);唯一构造器是 `hit_from_payload`(L108,调用点 L469/L505/L551,全覆盖 code 世界所有通道:精确标识符/向量/兜底 scroll)
- **doc 世界** `source_ref` = payload doc_id = `dt://doc/{project}/{rel}`(make_document_id, src/domain/id.rs:50);配置文档的 doc_id 带 `#section-xxx` 锚点
- **knowledge 世界** `source_ref` 由 fill_source_refs(retrieve.rs:984)从 `MENTIONED_IN` 的 Document.doc_id 回填 → 同样是 `dt://doc/{project}/{rel}`,同一解析路径
- Qdrant 所有集合 payload 都带 `project` 标签(collections.rs 注释「项目作为 payload 标签」)
- `load_config()`(main.rs:426)/`resolve_project_paths()`(main.rs:491)→ `Vec<(String, PathBuf)>`;main.rs 是 **bin target**、build.rs 在 **lib target** —— **lib 侧无法调用 bin 私有 fn,项目映射必须由 main.rs 算好作为参数传入**
- `render_human`(search_render.rs:80)唯一外部调用者 = build.rs:807(grpc 走 build_service.rs 自己的 handle_search,不经过它)
- grpc `hit_to_proto`(build_service.rs:200)只**读** SearchHit 字段 → 给 SearchHit 加字段不破坏 grpc 编译;proto 不映射新字段(避免 proto IDL 变更,现状 MCP 走 `dt search --json` 子进程,本就不经过 grpc)
- 全库 **11 处 `SearchHit {` 字面量构造**:search_mcp.rs:662/733/948/1031、retrieve.rs:1146/2028、fusion.rs:90、search_config.rs:160、search_memory.rs:29、build_service.rs:330、search_render.rs:104 —— 加字段时每处补 `project: None`(⚠️ 逐个改,勿正则批量插入,见 SKILL.md 中 file_type 字段的历史教训)

## dt:// URI 解析规则

- 只解析 `dt://doc/` 前缀:`strip_prefix("dt://doc/")` → `split_once('/')` → (project, rel)。项目别名单段(不含 `/`,config 映射 key 与目录名都不含),split_once 安全;rel 可含多级目录
- rel 可能带 `#section-...` 锚点,先 `split('#').next()` 剥离再 join 到项目根
- **跳过前缀(原样保留)**:`dt://entity/`、`dt://nacos/`、`dt://config/`、`dt://jenkins/`、`dt://event/` — 无磁盘路径
- **查找失败 → 保留原值,绝不置空**:项目不在 config、旧数据无项目段(如 `dt://doc/支付架构决策.md`)、config 本身缺失

## 推荐实施:展示层(4 处改动)

1. **main.rs:1207 附近**(Search 命令臂):`let project_paths = load_config().map(|cfg| resolve_project_paths(&cfg).into_iter().collect::<std::collections::HashMap<String, std::path::PathBuf>>());` 作为新参数传 handle_search。config 加载失败 → None → 渲染零变化(零回归设计)
2. **build.rs:757-768** 签名加 `project_paths: Option<std::collections::HashMap<String, std::path::PathBuf>>`;L807 传给 render_human
3. **search_render.rs:L80/L15** 签名加参;L58-66 位置/来源块解析;新增纯函数:
   ```rust
   fn resolve_doc_uri(uri: &str, map: Option<&HashMap<String, PathBuf>>) -> Option<String> {
       let rest = uri.strip_prefix("dt://doc/")?;
       let (project, rel) = rest.split_once('/')?;
       let base = map?.get(project)?;
       let rel = rel.split('#').next().unwrap_or(rel);
       Some(base.join(rel).to_string_lossy().into_owned())
   }
   ```
   code 世界:`h.project.as_deref()` 查 map → `base.join(file_path)`,查不到保留相对路径
4. **SearchHit 加字段**:`#[serde(default, skip_serializing_if = "Option::is_none")] pub project: Option<String>`;`hit_from_payload` 里 `project: payload.get("project").and_then(|v| v.as_str()).map(|s| s.to_string())`

**code 世界替代方案(零结构改动,不推荐为主)**:不从 payload 读 project,而在展示层从 `element_id`(method_id = `dt://entity/{project}/class/...`)解析 project —— 省 11 处字面量改动,但依赖 method_id 格式且 element_id 可能缺失。

## 风险点

- **别名不一致**:payload project = 构建时 `--name`(可能 ≠ config 别名,如直接用目录名构建)→ 查不到 → 优雅回退原值,不会显示错误路径
- **JSON 增量**:`skip_serializing_if` 必须带,否则非 code 命中多出 `"project": null`;带上后 code 命中 JSON 仅多 `"project": "pay-center"`(纯增量,MCP serde 忽略未知字段)
- **`#section-` 锚点显示丢失**:决策点;若用户要保留,渲染成 `{磁盘路径} (#section-spring.cloud)`
- 解析失败只 `tracing::debug!`(不 warn)—— mcp-server.py 的 run_cmd 合并 stdout+stderr,warn 会污染 `--json` 输出
- **数据层方案不推荐**(改 postprocess_hits 里的 source_ref 值):需把映射灌进 CrossWorldSearch 结构/postprocess_hits 签名 → `new()` 全部调用点(build.rs:781、grpc、daemon、测试)全改,且 JSON/MCP 契约变更(消费 `dt://` URI 的工具会坏)。除非用户明确要求 JSON 也显示磁盘路径

## 测试计划

- search_render.rs 新增纯函数测试(无后端):已知项目解析、`#section-` 剥离、未知项目/旧格式保留原值、非 doc 前缀不动、code project 解析、端到端 map 有无对比
- search_mcp.rs:`hit_from_payload_carries_project`(payload 带 project → hit.project == Some)
- 同步:现有 7 处 `render_human(&x, false)` → 加 `, None`;11 处 SearchHit 字面量补字段;现有断言(如 `来源: dt://doc/支付架构决策.md`、nacos 来源)在 None map 下仍然通过
- 验证顺序(per skill):`cargo test --release --lib <filter>` 一次一个 filter → `cargo fmt --check` → `cargo check --release` → `git diff --check`;手动冒烟 `dt search --world doc/code` 看路径变磁盘、`--json` 仍是 dt:// URI
- 实施前 `git status` 确认 src/ 干净(2026-08-11 时 worktree 有无关改动:config/pipeline.yaml、skill/)
