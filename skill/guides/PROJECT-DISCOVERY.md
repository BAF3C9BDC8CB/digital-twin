# 项目发现规则

> AI 每次进入新工作空间时执行此规则。不在代码中实现，由 AI 按本文档自行判断。
> **注意：项目注册表 = `~/.config/digital-twin/config.yaml` 的 `projects` 段（直接读文件，无 `dt list` 命令）；**
> **索引状态用 `dt_health` 查看后端健康、用 `dt search "<类名>" --json` 验证是否已有索引；构建用 `dt build`（无参数 = 构建所有项目）或 MCP `dt_build`。**

---

## 触发时机

用户用 opencode 打开一个目录，或切换到新工作目录时：

**第一步：快速诊断已有项目**

```bash
# 1. 读项目注册表
cat ~/.config/digital-twin/config.yaml   # projects 段：base + name 列表

# 2. 验证某项目是否已索引（能搜到方法即已索引）
dt search "<已知类名或方法名>" --project <项目名> --json
```

对照判断当前目录下的项目：
- **已在 config.yaml 注册且已有索引** → 一切正常，跳过
- **已注册但未 build**（无向量/方法数据）→ 提示用户执行 `dt build --name <项目名>`（或 MCP `dt_build`）
- **未注册** → 执行项目发现

**第二步：检查当前工作目录**

1. 检查 config.yaml `projects` 中是否存在当前工作目录或其父目录
2. 如果**根目录已注册且有索引** → 跳过项目发现
3. 如果**根目录已注册但无索引** → 提示 `dt build --name <项目名>`
4. 如果**根目录未注册** → 执行项目发现

---

## 项目识别规则

**递归扫描当前目录，任何子目录满足以下条件即为「项目候选」：**

```
该目录下（不限深度）存在至少 1 个源码文件
源码文件扩展名：
  .java  .py  .ts  .tsx  .js  .jsx  .mjs  .cjs
  .go   .rs  .php  .vue  .html  .css  .scss  .less
```

**注意：**
- **不需要** `pom.xml` / `package.json` / `go.mod` 等构建标识
- **不限扫描深度** — 多级嵌套仓库（如 `warehouse/goods/uvp-goods-center`）也要发现
- 一个子目录只要源码文件数 > 0 就是候选，不管文件多少

---

## 去重规则

从候选列表中排除：

| 排除条件 | 判断方式 |
|---------|---------|
| 已在 `config.yaml` 注册 | `~/.config/digital-twin/config.yaml` projects 段中已列出 |
| 已知非项目目录 | `scanner.ignore_dirs` 中列出的目录（.git .weave node_modules ...） |
| 用户已拒绝且不再询问 | `ignored_dirs.yaml` 中列出的路径 |
| 已有索引 | `dt search "<关键词>" --project <项目名>` 能搜到方法 |

---

## 已提示标记

不需要额外追踪状态。使用两个文件记录：

| 文件 | 路径 | 作用 |
|------|------|------|
| `config.yaml` | `~/.config/digital-twin/config.yaml` | 用户选"是" → 项目写入此文件 → 下次不再问 |
| `ignored_dirs.yaml` | `~/.config/digital-twin/ignored_dirs.yaml` | 用户选"否，不再询问" → 目录写入此文件 → 永久跳过 |

`ignored_dirs.yaml` 格式：
```yaml
# 不再提示项目发现的目录（用户选择了"否，不再询问"）
ignored:
  - /data/aflmProjects/some-legacy-repo
  - /data/myProject/third-party-tools
```

如果文件不存在 → 视为空列表，无任何目录被忽略。

---

## 提示模板

发现 N 个未注册项目时，必须用 `question` 工具提示用户（不能静默操作）：

```
检测到 N 个未索引的项目，它们在当前目录下有源码文件但尚未加入向量库：

  uvp-business-center    → /data/aflmProjects/warehouse/uvp-business-center
  uvp-warehouse-api      → /data/aflmProjects/warehouse/uvp-warehouse-api
  ...

是否添加到 config.yaml 并执行 dt build？

选项：
  [是，全部添加并构建]        → 追加到 config.yaml → `dt build`（默认构建所有项目）
  [是，仅添加不构建]          → 只追加到 config.yaml，不 build
  [否，下次再说]              → 不做任何操作，下次打开仍会提示
  [否，不再询问此目录]        → 写入 ignored_dirs.yaml，永久跳过
  [让我手动选择...]           → 逐个勾选
```

---

## 操作流程

### 用户选"是"时

```bash
# 1. 追加到 config.yaml projects 列表末尾
# 2. 执行构建（无参数 = 构建 config.yaml 中所有项目；
#    单项目可用 dt build --name <项目名> 或 MCP dt_build(path=..., name=...)）
dt build
```

### 用户选"否，不再询问"时

```bash
# 将当前工作目录写入 ignored_dirs.yaml
# 如文件不存在则创建
```

### 用户选"手动选择"时

逐个弹出项目让用户勾选，选中项追加到 config.yaml，然后提供是否立即 build 的选项。

---

## 示例

```
工作目录: /data/aflmProjects/warehouse

1. 读 config.yaml projects 段 + dt search 抽查索引:
   已注册且有索引: yyc-caigou, yyc-yaochang-gongsi
   已注册但无索引: warehouse
   （uvp-business-center 等未列出 → 未注册）

2. 检查 ignored_dirs.yaml:
   不存在 → 无忽略项

3. 递归扫描（跳过 ignore_dirs）:
   发现源码的目录:
   ├── uvp-business-center/       ★ 候选 (未注册)
   ├── uvp-warehouse-api/         ★ 候选
   ├── uvp-warehouse-center/      ★ 候选
   ├── yyc-caigou/                ✗ 已注册且有索引
   ├── yyc-yaochang-gongsi/       ✗ 已注册且有索引
   └── goods/
       ├── goods-center-h5/        ★ 候选
       └── uvp-goods-center/       ★ 候选

4. 候选数 5 → 提示用户
```
