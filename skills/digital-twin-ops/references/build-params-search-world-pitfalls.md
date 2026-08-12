# dt build 参数陷阱 + 检索 world 参数 — 2026-08-12 团队测试发现

## 1. --file 单文件构建误删 Memgraph 方法（pipeline.rs 已修复）

### 症状
`dt build --path X --file GroupService.java` 后，Memgraph 中该项目的
Method 数从 2287 崩到 11（只剩被构建的那 1 个文件的 11 个方法）。

### 根因（pipeline.rs 步骤 2/3）
- 步骤 1：`--file X` 时 `all_files = vec![X]`（只含目标文件）
- 步骤 2：IncrementalStrategy.select_files 用快照（342 个文件）对比 current（1 个）
  → **其余 341 个文件全部被误判为 deleted**
- 步骤 3：`delete_files_from_graph(project, deleted)` 把 341 个文件的方法全部删掉

### 修复
单文件模式跳过 select_files 的 deleted 检测：
```rust
let (files_to_process, deleted) = match &self.target_file {
    Some(_) => (all_files.clone(), Vec::new()),
    None => strategy.select_files(...).await?,
};
```

### 判断特征
- Qdrant 向量数正常（2287 仍在，只删了 Memgraph）
- sense 的 methods 从 Memgraph 读 → 崩到个位数
- daemon.log 显示 "构建完成: 扫描 1 个文件, 变更 1 个, 共 11 个方法"

## 2. dt build 项目名参数：--name 才是注册名，--path 取目录名

- `dt build --path /data/aflmProjects/aflm/uvp-im-center` → project_name =
  目录名 **uvp-im-center**（错误！写入新节点）
- `dt build --name im-center` → 从 config.yaml 解析注册名 **im-center**（正确）

误用 --path 会在 Memgraph 留下 `uvp-im-center` 死节点（Project + 方法），
污染按项目过滤的检索。清理需 DETACH DELETE（MCP memgraph 只读时用
`MCP_READ_ONLY=false` 或用 dt clean，但 dt clean 只支持全量）。

**规则**：构建已注册项目一律 `dt build --name <注册名> [--full]`，不要传 --path。

## 3. ~~dt_search_kg 硬编码 knowledge world → 代码检索 0% 命中~~（2026-08-12 已修复：支持 world/project 参数，见 kg-query-strategy.md）

### 症状
`dt_search_kg(query="sendMessage")` 对 im-center 检索命中率 0%，
结果全被 message-center 污染。

### 根因
mcp-server.py 的 dt_search_kg 固定传 `--world knowledge`（只含配置/服务实体，
约 55 个节点），而代码实体（Class/Method）在 code world。

### 修复（mcp-server.py 2026-08-12）
dt_search_kg 增加 `world` + `project` 参数：
- 默认 `world=knowledge`（向后兼容）
- 检索代码实体必须传 `world="code"` + `project="im-center"`
- CLI 等价：`dt search "发送单聊消息" --world code --project im-center`

### 验证
`dt search "sendMessage" --world code --project im-center` → 3/3 命中
im-center 真实实体（MessageService/ImClient/MessageController），
llm_analysis 描述准确。查询词用英文方法名（accountImport）比中文
（导入账号）命中更稳——向量检索对精确标识符召回更好。

## 4. dt build 是 daemon 化进程（调试陷阱）

- `dt build` 的 stdout/stderr 基本为空（INFO 走 /var/log/digital-twin/dt.log）
- 判断构建是否执行：看 daemon.log 的 "开始构建: project=X, full=…" 与
  "构建完成: 扫描 N 个文件" 行
- 在解析器加 `eprintln!` DBG 可能看不到（若构建走了流水线/TsJavaParser 而非
  你改的 JavaParser——见 treesitter-comment-extraction-bug.md）
- **绝不允许并行 dt build**：并发构建互杀，Phase2 中断 → 大量 MISSING

## 5. 团队测试工作流（本项目 KG 验收范式）

用户要求"组建两个团队"测试项目时：
- 团队 A（3 角色）：架构分析 / 功能测试 / 外部依赖与数据流
- 团队 B（3 角色）：KG 使用审计 / 检索质量验证 / 索引诊断
- 两团队并行派出（delegate_task tasks=[...]），汇总后按 B 的问题清单
  实施修复 → 重建 → 复测，直到全部通过
- 报告与改进记录存 `reports/2026-08-12-imcenter-team-test-kg-improve.md`（示例）
