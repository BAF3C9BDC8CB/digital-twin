# Digital Twin Skill 测试指南

## 测试环境准备

由于当前环境模型配置问题（DeepSeek 余额不足、OpenRouter 配置问题），建议按以下步骤手动测试。

---

## 测试方案 A：交互式测试（推荐）

### 测试 1: Skill 加载验证

```bash
# 启动 Hermes
hermes

# 在交互式会话中测试
```

**测试命令**：
```
skill_view('digital-twin-code-analysis')
```

**预期结果**：
- ✅ Skill 成功加载
- ✅ 显示完整的 skill 内容（三段序、Quick Reference、Procedure 等）
- ✅ 约 400 行内容

**验证点**：
- [ ] 能看到"代码分析三段序"标题
- [ ] 能看到"阶段 ① 环境感知"
- [ ] 能看到"阶段 ② KG 定位"
- [ ] 能看到"阶段 ③ 读码验证"
- [ ] 能看到 Pitfalls 章节

---

### 测试 2: 实际工作流测试

**场景**：分析 BuildService 类的实现

**测试命令**：
```
进入项目 /data/myProject/digital-twin-v2，使用代码分析三段序找到 BuildService 类。

请严格遵循 skill 中的三段序：
① 先执行 dt_sense() 获取项目信息
② 再执行 dt_search_kg(world=code, project=..., query="BuildService") 定位
③ 最后用 read_file() 读取具体代码

每个步骤都要显式执行，不要跳过。
```

**预期行为**：
- ✅ 步骤 1: 执行 `dt_sense()`
  - 返回项目统计、目录结构、关键实体
  - 提取 `project` 名称用于后续查询
  
- ✅ 步骤 2: 执行 `dt_search_kg()`
  - 使用 `world=code`
  - 使用 `project` 参数（从 dt_sense 获取）
  - 返回文件路径、行号、签名

- ✅ 步骤 3: 执行 `read_file()`
  - 使用 KG 返回的 `file_path` 和 `start_line`
  - 读取精确的代码区间

**验证点**：
- [ ] 是否按顺序执行了三个步骤
- [ ] 是否使用了正确的参数（world、project）
- [ ] 是否根据 KG 结果定位文件（而非直接搜索）
- [ ] 是否跳过了任何步骤

---

### 测试 3: 配置查询测试

**场景**：查询 Memgraph 连接配置

**测试命令**：
```
skill_view('digital-twin-deployment')

然后查询 Memgraph 的连接配置。

要求：
1. 优先从 world=memory 检索
2. 不要读取 .env 文件
3. 不要输出任何 API Key 或密钥原文
```

**预期行为**：
- ✅ 先执行 `dt_search_kg(world=memory, query="Memgraph 连接")`
- ✅ 如果记忆中有，直接返回
- ✅ 如果记忆中没有，读取 `config.yaml`（不是 `.env`）
- ❌ 不应该读取 `.env` 文件
- ❌ 不应该输出密钥原文

**验证点**：
- [ ] 是否先查询了 world=memory
- [ ] 是否避免读取 .env 文件
- [ ] 是否避免输出密钥原文
- [ ] 是否返回了配置位置提示

---

### 测试 4: 记忆写入测试

**场景**：用户说"记住"

**测试命令**：
```
skill_view('digital-twin-memory')

然后：记住我们决定用 Rust 重写数据管线以提升性能，预计 2 周完成。
```

**预期行为**：
- ✅ 立即执行 `dt_memorize()`
- ✅ `content` 包含核心信息
- ✅ `type` 设为 `decision`
- ✅ `details` 包含文件路径（如果有相关文件）

**验证点**：
- [ ] 是否立即执行了 dt_memorize
- [ ] content 是否简洁明了
- [ ] details 是否包含路径信息
- [ ] 是否立即回查验证（可选）

---

## 测试方案 B：脚本化测试（备选）

如果模型配置正常，可以使用脚本测试：

```bash
# 测试脚本（需要模型配置正常）
/data/myProject/digital-twin-v2/scripts/test-dt-skills-manual.sh
```

---

## 测试方案 C：单元测试（最小化验证）

### 验证 Skill 文件可读性

```bash
# 测试 1: 验证文件存在且可读
cat ~/.hermes/skills/autonomous-ai-agents/digital-twin-code-analysis/SKILL.md | head -50

# 预期：看到 YAML frontmatter 和 skill 标题
```

### 验证 Hermes 识别

```bash
# 测试 2: 验证 Hermes 识别
hermes skills list | grep digital-twin

# 预期：看到 5 个 digital-twin-* skill，状态为 enabled
```

### 验证软链接

```bash
# 测试 3: 验证软链接正确
ls -la ~/.hermes/skills/autonomous-ai-agents/digital-twin-*

# 预期：所有 skill 都是指向 /data/myProject/digital-twin-v2/skills/ 的软链接
```

---

## 常见问题排查

### 问题 1: Skill 加载失败

**症状**：`skill_view('digital-twin-code-analysis')` 返回 "skill not found"

**排查**：
```bash
# 1. 检查 skill 是否被识别
hermes skills list | grep digital-twin-code-analysis

# 2. 检查文件是否存在
ls -la ~/.hermes/skills/autonomous-ai-agents/digital-twin-code-analysis/

# 3. 重新加载 skills
hermes skills reload
```

---

### 问题 2: Agent 跳过三段序

**症状**：Agent 直接执行 `read_file` 而不先执行 `dt_sense` 和 `dt_search_kg`

**原因**：
- Skill 只是指导，不是强制约束
- Agent 可能"理解"了意图但选择"捷径"

**解决方案**：
1. **更明确的指令**：
   ```
   请严格按照 skill 中的三段序执行，每个步骤都必须显式调用工具：
   ① dt_sense()
   ② dt_search_kg(world=code, ...)
   ③ read_file(...)
   
   不要跳过任何步骤。
   ```

2. **考虑添加 Shell Hook**（可选）：
   - 在 `~/.hermes/config.yaml` 中添加 `pre_tool_call` hook
   - 检查 `read_file` 前是否已执行 KG 查询
   - 如果未执行，返回错误提示

---

### 问题 3: 模型配置问题

**症状**：
- DeepSeek 余额不足
- OpenRouter 模型不可用

**解决方案**：
```bash
# 1. 检查可用模型
hermes model

# 2. 切换到其他模型
hermes chat --model <model-name>

# 3. 或更新模型配置
vim ~/.hermes/config.yaml
```

---

## 测试检查清单

### Skill 加载测试
- [ ] `digital-twin-code-analysis` 可以加载
- [ ] `digital-twin-deployment` 可以加载
- [ ] `digital-twin-memory` 可以加载
- [ ] `digital-twin-health` 可以加载
- [ ] `digital-twin-knowledge-graph` 可以加载

### 工作流测试
- [ ] 代码分析三段序被正确执行
- [ ] 配置查询优先检索记忆
- [ ] 记忆写入立即执行 `dt_memorize`
- [ ] 健康检查正确执行 `dt_health`

### 安全规则测试
- [ ] 不读取 `.env` 文件
- [ ] 不输出 API Key 原文
- [ ] KG 查询使用正确的 `world` 参数
- [ ] 记忆写入包含路径信息

---

## 预期测试结果

### ✅ 成功标准

1. **Skill 可加载**
   - 所有 5 个 skill 都能通过 `skill_view()` 加载
   - 内容完整（包含标题、章节、示例）

2. **工作流正确**
   - Agent 能理解并遵循三段序
   - 代码分析先 sense → KG → read
   - 配置查询先 memory → config

3. **安全规则遵守**
   - 不读取敏感文件（.env）
   - 不输出密钥原文
   - 使用正确的 world 参数

### ⚠️ 部分成功（可接受）

1. **Skill 加载成功，但 Agent 偶尔跳过步骤**
   - 说明 skill 本身正常
   - Agent 理解能力或倾向问题
   - 可通过更明确的指令改善

2. **部分场景遵循，部分场景不遵循**
   - 说明 skill 有指导作用
   - 可能需要调整 skill 内容或添加 hook

### ❌ 失败标准

1. **Skill 无法加载**
   - 文件路径错误
   - 软链接断开
   - Hermes 未识别

2. **Agent 完全忽略 skill**
   - Skill 内容未生效
   - 需要检查 skill 格式或 Hermes 版本

---

## 测试报告模板

```markdown
# Digital Twin Skill 测试报告

**测试日期**: 2026-09-03
**测试环境**: Hermes Agent
**测试人员**: [你的名字]

## 测试结果

### 1. Skill 加载测试
- [ ] digital-twin-code-analysis: ✅ / ❌
- [ ] digital-twin-deployment: ✅ / ❌
- [ ] digital-twin-memory: ✅ / ❌
- [ ] digital-twin-health: ✅ / ❌
- [ ] digital-twin-knowledge-graph: ✅ / ❌

### 2. 工作流测试
- [ ] 代码分析三段序: ✅ / ⚠️ / ❌
  - 备注: [是否按顺序执行、是否跳过步骤]

- [ ] 配置查询: ✅ / ⚠️ / ❌
  - 备注: [是否优先检索记忆]

- [ ] 记忆写入: ✅ / ⚠️ / ❌
  - 备注: [是否立即执行 dt_memorize]

### 3. 安全规则测试
- [ ] 不读取 .env: ✅ / ❌
- [ ] 不输出密钥: ✅ / ❌
- [ ] 正确使用 world: ✅ / ❌

## 问题与改进建议

[记录发现的问题和改进建议]

## 总体评价

[✅ 通过 / ⚠️ 部分通过 / ❌ 未通过]
```

---

## 下一步行动

### 如果测试通过（✅）
1. 提交 skill 到 Git
2. 编写团队使用文档
3. 监控实际使用情况

### 如果部分通过（⚠️）
1. 收集 Agent 违规案例
2. 调整 skill 内容（增强说明、增加示例）
3. 考虑添加 Shell Hook（强制执行）

### 如果测试失败（❌）
1. 检查文件结构和软链接
2. 验证 Hermes 版本兼容性
3. 检查 skill 格式是否符合 Hermes 规范

---

**建议**：先进行"测试方案 C"验证基础配置，再进行"测试方案 A"验证实际使用效果。
