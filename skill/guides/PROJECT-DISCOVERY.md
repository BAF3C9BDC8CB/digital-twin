# 项目发现规则

> AI 每次进入新工作空间时执行此规则。不在代码中实现，由 AI 按本文档自行判断。

---

## 触发时机

用户用 opencode 打开一个目录，或切换到新工作目录时：

**第一步：快速诊断已有项目**

```bash
dt list --all
```

该命令输出 `config.yaml` 中所有注册项目的状态：
- **磁盘** — 路径是否存在
- **向量** — Qdrant 中是否有索引
- **方法** — Memgraph 中是否有知识图谱节点

如果当前目录下的项目**磁盘 ✅ 但向量为 0 或 -**，说明项目已注册但未 build，提示用户执行 `dt build-all`。
如果当前目录下的项目**磁盘 ✅ 向量有值**，说明一切正常，跳过。

**第二步：检查当前工作目录**

1. 检查 `dt list` 输出中是否存在当前工作目录或其父目录
2. 如果**根目录已注册且向量不为 0** → 跳过项目发现
3. 如果**根目录已注册但向量为 0** → 提示 `dt build-all`
4. 如果**根目录未注册**或`dt list` 输出显示当前目录下存在独立子项目未列出 → 执行项目发现

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
| 已在 `config.yaml` 注册 | `dt list` 中已列出 |
| 已知非项目目录 | `scanner.ignore_dirs` 中列出的目录（.git .weave node_modules ...） |
| 用户已拒绝且不再询问 | `ignored_dirs.yaml` 中列出的路径 |
| 已有向量索引 | `dt list --all` 输出中该项目向量数 > 0 |

---

## 已提示标记

不需要额外追踪状态。使用两个文件记录：

| 文件 | 路径 | 作用 |
|------|------|------|
| `config.yaml` | `~/.config/opencode/skills/digital-twin/config.yaml` | 用户选"是" → 项目写入此文件 → 下次不再问 |
| `ignored_dirs.yaml` | `~/.config/opencode/skills/digital-twin/ignored_dirs.yaml` | 用户选"否，不再询问" → 目录写入此文件 → 永久跳过 |

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
  [是，全部添加并构建]        → 追加到 config.yaml → dt build-all --filter "..."
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
# 2. 执行构建
dt build-all --filter "proj1,proj2,..."
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

1. dt list --all:
   项目名                 磁盘  向量    方法   路径
   yyc-caigou            ✅    1089   1089   /data/.../yyc-caigou
   yyc-yaochang-gongsi   ✅    583    583    /data/.../yyc-yaochang-gongsi
   warehouse             ✅    0      13066  /data/.../warehouse
   （uvp-business-center 等未列出 → 未注册）

2. 检查 ignored_dirs.yaml:
   不存在 → 无忽略项

3. 递归扫描（跳过 ignore_dirs）:
   发现源码的目录:
   ├── uvp-business-center/       ★ 候选 (不在 dt list 中)
   ├── uvp-warehouse-api/         ★ 候选
   ├── uvp-warehouse-center/      ★ 候选
   ├── yyc-caigou/                ✗ 已有向量 (dt list --all 中向量>0)
   ├── yyc-yaochang-gongsi/       ✗ 已有向量
   └── goods/
       ├── goods-center-h5/        ★ 候选
       └── uvp-goods-center/       ★ 候选

4. 候选数 5 → 提示用户
```
