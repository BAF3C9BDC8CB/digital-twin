# Hermes Agent 中文文档

这是从 [Hermes Agent 官方文档](https://hermes-agent.nousresearch.com/docs/zh-Hans/) 抓取的中文文档离线版本。

抓取时间：2026-09-03

## 目录结构

### 📚 Getting Started (入门指南)

- [学习路径](getting-started/learning-path.md) - 根据经验水平和目标的学习路径指南
- [快速入门](getting-started/quickstart.md) - 快速开始使用 Hermes Agent
- [安装](getting-started/installation.md) - 安装指南
- [平台支持](getting-started/platform-support.md) - 支持的平台信息
- [Android / Termux](getting-started/termux.md) - 在 Android Termux 环境中安装
- [Nix & NixOS 安装配置](getting-started/nix-setup.md) - Nix 系统配置
- [更新与卸载](getting-started/updating.md) - 如何更新和卸载

### 👤 User Guide (用户指南)

#### 基础功能
- [CLI 用法](user-guide/cli.md) - 命令行界面使用指南
- [配置](user-guide/configuration.md) - 配置选项说明
- [会话](user-guide/sessions.md) - 会话管理
- [消息](user-guide/messaging.md) - 消息平台集成概览
- [安全](user-guide/security.md) - 安全相关配置

#### 功能特性 (Features)
- [工具](user-guide/features/tools.md) - 内置工具说明
- [技能](user-guide/features/skills.md) - 技能系统
- [记忆](user-guide/features/memory.md) - 持久化记忆功能
- [上下文文件](user-guide/features/context-files.md) - 上下文文件管理
- [MCP（模型上下文协议）](user-guide/features/mcp.md) - MCP 集成
- [Cron](user-guide/features/cron.md) - 定时任务调度
- [委派](user-guide/features/delegation.md) - 子代理委派
- [代码执行](user-guide/features/code-execution.md) - Python 代码执行
- [浏览器](user-guide/features/browser.md) - 网页浏览功能
- [Hooks](user-guide/features/hooks.md) - 事件钩子系统
- [批处理](user-guide/features/batch-processing.md) - 批量处理功能
- [Provider 路由](user-guide/features/provider-routing.md) - LLM Provider 路由
- [插件](user-guide/features/plugins.md) - 插件系统
- [语音模式](user-guide/features/voice-mode.md) - 语音交互模式

#### 消息平台 (Messaging)
- [Telegram 配置](user-guide/messaging/telegram.md) - Telegram 机器人配置
- [Discord 配置](user-guide/messaging/discord.md) - Discord 机器人配置

### 🛠️ Developer Guide (开发者指南)

- [架构](developer-guide/architecture.md) - 系统架构说明
- [添加工具](developer-guide/adding-tools.md) - 如何添加新工具
- [创建技能](developer-guide/creating-skills.md) - 如何创建自定义技能
- [构建插件](developer-guide/plugins.md) - 插件开发指南
- [贡献指南](developer-guide/contributing.md) - 如何为项目做贡献

### 📖 Guides (教程与指南)

- [技巧与窍门](guides/tips.md) - 使用技巧集锦
- [Python 库指南](guides/python-library.md) - 作为 Python 库使用
- [在 Hermes 中使用语音模式](guides/use-voice-mode-with-hermes.md) - 语音模式使用教程
- [每日简报机器人](guides/daily-briefing-bot.md) - 示例项目：每日简报机器人
- [团队 Telegram 助手](guides/team-telegram-assistant.md) - 示例项目：团队助手

## 统计信息

- **总文档数**: 38 个 Markdown 文件
- **目录结构**: 7 个主要目录
- **覆盖范围**: 
  - 入门指南: 7 篇
  - 用户指南: 21 篇
  - 开发者指南: 5 篇
  - 教程指南: 5 篇

## 使用建议

根据您的需求，推荐的阅读路径：

### 🚀 初学者路径
1. [快速入门](getting-started/quickstart.md)
2. [CLI 用法](user-guide/cli.md)
3. [配置](user-guide/configuration.md)
4. [工具](user-guide/features/tools.md)

### 🤖 机器人部署路径
1. [安装](getting-started/installation.md)
2. [消息概览](user-guide/messaging.md)
3. [Telegram 配置](user-guide/messaging/telegram.md) 或 [Discord 配置](user-guide/messaging/discord.md)
4. [安全](user-guide/security.md)

### 🔧 开发者路径
1. [架构](developer-guide/architecture.md)
2. [构建插件](developer-guide/plugins.md)
3. [创建技能](developer-guide/creating-skills.md)
4. [贡献指南](developer-guide/contributing.md)

## 注意事项

- 本文档为离线版本，可能与在线版本存在差异
- 建议定期访问 [官方文档](https://hermes-agent.nousresearch.com/docs/zh-Hans/) 获取最新更新
- 部分页面（如 rl-training）可能在抓取时不存在，未包含在本文档集中

## 相关链接

- 官方网站: https://hermes-agent.nousresearch.com/
- GitHub 仓库: https://github.com/NousResearch/hermes-agent
- Discord 社区: https://discord.gg/NousResearch
- 技能中心: https://agentskills.io/

---

生成时间: 2026-09-03  
生成方式: 使用 Python + BeautifulSoup 批量抓取
