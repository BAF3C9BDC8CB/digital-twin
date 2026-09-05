#!/usr/bin/env python3
"""
批量抓取 Hermes Agent 中文文档
"""
import json
import time
import os
from pathlib import Path

# 所有需要抓取的文档页面
DOCS = {
    # Getting Started
    "getting-started/quickstart": "hermes-docs-zh/getting-started/quickstart.md",
    "getting-started/installation": "hermes-docs-zh/getting-started/installation.md",
    "getting-started/platform-support": "hermes-docs-zh/getting-started/platform-support.md",
    "getting-started/termux": "hermes-docs-zh/getting-started/termux.md",
    "getting-started/nix-setup": "hermes-docs-zh/getting-started/nix-setup.md",
    "getting-started/updating": "hermes-docs-zh/getting-started/updating.md",
    
    # User Guide
    "user-guide/cli": "hermes-docs-zh/user-guide/cli.md",
    "user-guide/configuration": "hermes-docs-zh/user-guide/configuration.md",
    "user-guide/sessions": "hermes-docs-zh/user-guide/sessions.md",
    "user-guide/messaging": "hermes-docs-zh/user-guide/messaging.md",
    "user-guide/security": "hermes-docs-zh/user-guide/security.md",
    
    # User Guide - Features
    "user-guide/features/tools": "hermes-docs-zh/user-guide/features/tools.md",
    "user-guide/features/skills": "hermes-docs-zh/user-guide/features/skills.md",
    "user-guide/features/memory": "hermes-docs-zh/user-guide/features/memory.md",
    "user-guide/features/context-files": "hermes-docs-zh/user-guide/features/context-files.md",
    "user-guide/features/mcp": "hermes-docs-zh/user-guide/features/mcp.md",
    "user-guide/features/cron": "hermes-docs-zh/user-guide/features/cron.md",
    "user-guide/features/delegation": "hermes-docs-zh/user-guide/features/delegation.md",
    "user-guide/features/code-execution": "hermes-docs-zh/user-guide/features/code-execution.md",
    "user-guide/features/browser": "hermes-docs-zh/user-guide/features/browser.md",
    "user-guide/features/hooks": "hermes-docs-zh/user-guide/features/hooks.md",
    "user-guide/features/batch-processing": "hermes-docs-zh/user-guide/features/batch-processing.md",
    "user-guide/features/rl-training": "hermes-docs-zh/user-guide/features/rl-training.md",
    "user-guide/features/provider-routing": "hermes-docs-zh/user-guide/features/provider-routing.md",
    "user-guide/features/plugins": "hermes-docs-zh/user-guide/features/plugins.md",
    "user-guide/features/voice-mode": "hermes-docs-zh/user-guide/features/voice-mode.md",
    
    # User Guide - Messaging
    "user-guide/messaging/telegram": "hermes-docs-zh/user-guide/messaging/telegram.md",
    "user-guide/messaging/discord": "hermes-docs-zh/user-guide/messaging/discord.md",
    
    # Developer Guide
    "developer-guide/architecture": "hermes-docs-zh/developer-guide/architecture.md",
    "developer-guide/adding-tools": "hermes-docs-zh/developer-guide/adding-tools.md",
    "developer-guide/creating-skills": "hermes-docs-zh/developer-guide/creating-skills.md",
    "developer-guide/contributing": "hermes-docs-zh/developer-guide/contributing.md",
    "developer-guide/plugins": "hermes-docs-zh/developer-guide/plugins.md",
    
    # Guides
    "guides/tips": "hermes-docs-zh/guides/tips.md",
    "guides/use-voice-mode-with-hermes": "hermes-docs-zh/guides/use-voice-mode-with-hermes.md",
    "guides/daily-briefing-bot": "hermes-docs-zh/guides/daily-briefing-bot.md",
    "guides/team-telegram-assistant": "hermes-docs-zh/guides/team-telegram-assistant.md",
    "guides/python-library": "hermes-docs-zh/guides/python-library.md",
}

BASE_URL = "https://hermes-agent.nousresearch.com/docs/zh-Hans/"

def main():
    """主函数：生成 URL 列表供 Chrome DevTools 工具使用"""
    
    # 创建目录结构
    for output_path in DOCS.values():
        Path(output_path).parent.mkdir(parents=True, exist_ok=True)
    
    # 输出 URL 列表
    print("需要抓取的文档页面：")
    print("=" * 80)
    for path, output in DOCS.items():
        url = BASE_URL + path
        print(f"{url} -> {output}")
    
    # 输出 JSON 格式供程序使用
    print("\n" + "=" * 80)
    print("\nJSON格式:")
    urls_json = [{"url": BASE_URL + path, "output": output} for path, output in DOCS.items()]
    print(json.dumps(urls_json, ensure_ascii=False, indent=2))
    
    print(f"\n总计: {len(DOCS)} 个页面")

if __name__ == "__main__":
    main()
