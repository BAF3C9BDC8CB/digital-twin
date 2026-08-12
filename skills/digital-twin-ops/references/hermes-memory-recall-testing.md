# KG 记忆召回测试配方（四层验证）

脚本：`~/.hermes/plugins/digital-twin/tests/test-recall.sh`
用法：`bash test-recall.sh "<查询词>"`（可传多个；EXPECT_WORD 环境变量指定 L3 期望关键词，默认 8095）

## 四层验证

| 层 | 方法 | 验证什么 | 通过标准 |
|----|------|---------|---------|
| L0 数据层 | `dt search "<q>" --world knowledge --limit 3 --json` | KG 里有没有可召回的向量点 | hits 非空 |
| L1 插件层 | `load_memory_provider('digital-twin')` + `is_available()` | 插件能否被发现/加载 | 加载 OK + available=True |
| L2 系统层 | `MemoryManager().add_provider(p)` + `prefetch_all(q)` | 注入文本是否生成 | 输出含 `[KG 记忆]` 块 |
| L3 端到端 | `hermes chat -q "<q>，不要查任何工具，直接回答"` | 模型是否真看到注入（0 工具调用答出私有数据）| 回答含 KG 里的关键信息（如端口号）|

## 关键判定逻辑

- **L3 是终极证据**：0 工具调用 + 答出私有数据（如 "uvp-warehouse-center 核心后端 8095"）——deepseek 等模型不可能预知私有项目信息，答出即证明记忆注入进上下文
- **L3 不要 grep "[KG 记忆]" 文本**：模型有时在 reasoning 显式引用（"KG记忆里已经有相关信息:..."），有时直接使用不引用——判定用回答内容（期望关键词），不是引用格式
- **L0 优先用语义查询词**（"warehouse 项目的端口和测试库"），不要用记忆 id 查（那是数据存在性检查，不是召回测试）

## 快速手动验证（不跑脚本）

```python
import sys; sys.path.insert(0, '/home/luis/.hermes/hermes-agent')
from plugins.memory import load_memory_provider
p = load_memory_provider('digital-twin'); p.initialize('t', agent_context='primary')
print(p.prefetch('warehouse 端口'))  # 应输出 [KG 记忆] 块
```

## 坑

- Hermes venv python 路径：`/home/luis/.hermes/hermes-agent/venv/bin/python`（脚本里必须全路径，注意前导斜杠）
- L3 会话耗时 ~20-40s（模型推理），timeout 90s
- 召回质量依赖查询词与记忆内容语义匹配度；无关查询（"今天天气"）空召回是**正确行为**不是故障
- 跨项目知识（credentials/local-services 无 project）召回时 project 标识为空，属正常

## CLI 会话验证（不需要导出包！）

**Hermes 导出包（default.tar.gz）只含 feishu/gateway 会话 + API 失败 dump——CLI 会话（hermes chat -q）永远不在导出里**。用户反复导出看不到测试会话是导出范围限制，不是故障。

CLI 会话存本机 SQLite：`/home/luis/.hermes/state.db`，验证记忆注入直接查库：

```python
import sqlite3
con = sqlite3.connect('/home/luis/.hermes/state.db')
con.row_factory = sqlite3.Row
rows = con.execute("""
    SELECT role, substr(coalesce(api_content,''),1,600) AS api_content,
           substr(coalesce(reasoning,''),1,300) AS reasoning
    FROM messages WHERE session_id=? ORDER BY id
""", ('<session_id>',)).fetchall()
```

### api_content 字段 = 实际发给模型的字节（注入铁证）

- 注入的 prefetch 块以 `<memory-context>` 开头，含 `[KG 记忆]` 文本（dt search --world knowledge 召回结果渲染）
- 模型 reasoning 里若写"KG 记忆已经给出/直接注入了相关信息" = 注入被实际使用
- tools=0（tool_call_count=0）+ 答出 KG 私有数据 = 完整闭环

### 找测试会话

```sql
SELECT id, title, started_at, tool_call_count FROM sessions
WHERE started_at LIKE '2026-08-12%' ORDER BY started_at DESC
```
- 测试会话特征：title 是测试问题（如 "Warehouse项目服务端口"），tool_call_count=0
- 插件安装（12:54）后才有 [KG 记忆] 注入；之前会话 api_content 无 <memory-context> 块
