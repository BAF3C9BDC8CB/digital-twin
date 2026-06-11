---
name: digital-twin
description: 知识图谱查询 + Qdrant 语义代码搜索 + 事件写入规则
---

# digital-twin 技能

遇到以下场景，按对应文档执行：

| 场景 | 文档 |
|------|------|
| 查知识图谱（任何任务的第一个动作） | [KG-QUERY.md](./KG-QUERY.md) |
| 搜索代码逻辑、方法定位 | [CODE-SEARCH.md](./CODE-SEARCH.md) |
| 写入事件/知识/记忆，或结束会话 | [WRITE-EVENTS.md](./WRITE-EVENTS.md) |

---

## 禁止

- ❌ 不要问用户"要不要查知识图谱"——静默查询
- ❌ 不要每次对话全量扫代码
- ❌ 不要询问用户已知存在于知识图谱中的信息
