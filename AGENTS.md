# Knowledge Graph Behavior

This project uses a Neo4j knowledge graph for persistent memory.

## ⚠️ 必须先加载 digital-twin 技能

执行任何任务前，先调用 `skill` 工具加载 **digital-twin** 技能，获取完整指令后再按流程执行。仅靠本文件不够——skill 文件中包含最新的详细工作流。

## 唯一不查 KG 的情况

当前环境无任何项目上下文（刚启动、无目录、无打开的文件）且用户消息中也无任何关键词。除此以外都必须先查 KG。
