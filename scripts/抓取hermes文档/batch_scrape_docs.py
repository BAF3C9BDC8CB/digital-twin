#!/usr/bin/env python3
"""
批量抓取 Hermes Agent 中文文档并保存为 Markdown
使用 requests + beautifulsoup 直接抓取
"""
import requests
from bs4 import BeautifulSoup
from pathlib import Path
import time
import sys

# 所有需要抓取的文档页面
DOCS = [
    # Getting Started
    ("getting-started/quickstart", "hermes-docs-zh/getting-started/quickstart.md"),
    ("getting-started/installation", "hermes-docs-zh/getting-started/installation.md"),
    ("getting-started/platform-support", "hermes-docs-zh/getting-started/platform-support.md"),
    ("getting-started/termux", "hermes-docs-zh/getting-started/termux.md"),
    ("getting-started/nix-setup", "hermes-docs-zh/getting-started/nix-setup.md"),
    ("getting-started/updating", "hermes-docs-zh/getting-started/updating.md"),
    
    # User Guide
    ("user-guide/cli", "hermes-docs-zh/user-guide/cli.md"),
    ("user-guide/configuration", "hermes-docs-zh/user-guide/configuration.md"),
    ("user-guide/sessions", "hermes-docs-zh/user-guide/sessions.md"),
    ("user-guide/messaging", "hermes-docs-zh/user-guide/messaging.md"),
    ("user-guide/security", "hermes-docs-zh/user-guide/security.md"),
    
    # User Guide - Features
    ("user-guide/features/tools", "hermes-docs-zh/user-guide/features/tools.md"),
    ("user-guide/features/skills", "hermes-docs-zh/user-guide/features/skills.md"),
    ("user-guide/features/memory", "hermes-docs-zh/user-guide/features/memory.md"),
    ("user-guide/features/context-files", "hermes-docs-zh/user-guide/features/context-files.md"),
    ("user-guide/features/mcp", "hermes-docs-zh/user-guide/features/mcp.md"),
    ("user-guide/features/cron", "hermes-docs-zh/user-guide/features/cron.md"),
    ("user-guide/features/delegation", "hermes-docs-zh/user-guide/features/delegation.md"),
    ("user-guide/features/code-execution", "hermes-docs-zh/user-guide/features/code-execution.md"),
    ("user-guide/features/browser", "hermes-docs-zh/user-guide/features/browser.md"),
    ("user-guide/features/hooks", "hermes-docs-zh/user-guide/features/hooks.md"),
    ("user-guide/features/batch-processing", "hermes-docs-zh/user-guide/features/batch-processing.md"),
    ("user-guide/features/rl-training", "hermes-docs-zh/user-guide/features/rl-training.md"),
    ("user-guide/features/provider-routing", "hermes-docs-zh/user-guide/features/provider-routing.md"),
    ("user-guide/features/plugins", "hermes-docs-zh/user-guide/features/plugins.md"),
    ("user-guide/features/voice-mode", "hermes-docs-zh/user-guide/features/voice-mode.md"),
    
    # User Guide - Messaging
    ("user-guide/messaging/telegram", "hermes-docs-zh/user-guide/messaging/telegram.md"),
    ("user-guide/messaging/discord", "hermes-docs-zh/user-guide/messaging/discord.md"),
    
    # Developer Guide
    ("developer-guide/architecture", "hermes-docs-zh/developer-guide/architecture.md"),
    ("developer-guide/adding-tools", "hermes-docs-zh/developer-guide/adding-tools.md"),
    ("developer-guide/creating-skills", "hermes-docs-zh/developer-guide/creating-skills.md"),
    ("developer-guide/contributing", "hermes-docs-zh/developer-guide/contributing.md"),
    ("developer-guide/plugins", "hermes-docs-zh/developer-guide/plugins.md"),
    
    # Guides
    ("guides/tips", "hermes-docs-zh/guides/tips.md"),
    ("guides/use-voice-mode-with-hermes", "hermes-docs-zh/guides/use-voice-mode-with-hermes.md"),
    ("guides/daily-briefing-bot", "hermes-docs-zh/guides/daily-briefing-bot.md"),
    ("guides/team-telegram-assistant", "hermes-docs-zh/guides/team-telegram-assistant.md"),
    ("guides/python-library", "hermes-docs-zh/guides/python-library.md"),
]

BASE_URL = "https://hermes-agent.nousresearch.com/docs/zh-Hans/"

def extract_article_content(html):
    """从 HTML 中提取文章主要内容并转换为 Markdown"""
    soup = BeautifulSoup(html, 'html.parser')
    
    # 找到 article 或 main 标签
    article = soup.find('article') or soup.find('main')
    if not article:
        return None
    
    # 提取文本内容
    content = article.get_text(separator='\n', strip=True)
    return content

def fetch_and_save(url, output_path):
    """抓取单个页面并保存"""
    try:
        print(f"正在抓取: {url}")
        response = requests.get(url, timeout=30)
        response.raise_for_status()
        
        # 提取内容
        content = extract_article_content(response.text)
        if not content:
            print(f"  ⚠️  无法提取内容")
            return False
        
        # 保存文件
        output_file = Path(output_path)
        output_file.parent.mkdir(parents=True, exist_ok=True)
        output_file.write_text(content, encoding='utf-8')
        
        print(f"  ✓ 已保存到: {output_path}")
        return True
        
    except Exception as e:
        print(f"  ✗ 错误: {e}")
        return False

def main():
    """主函数"""
    print(f"开始批量抓取 {len(DOCS)} 个文档页面...")
    print("=" * 80)
    
    success_count = 0
    fail_count = 0
    
    for url_path, output_path in DOCS:
        full_url = BASE_URL + url_path
        
        if fetch_and_save(full_url, output_path):
            success_count += 1
        else:
            fail_count += 1
        
        # 避免请求过快
        time.sleep(0.5)
    
    print("=" * 80)
    print(f"\n完成! 成功: {success_count}, 失败: {fail_count}")

if __name__ == "__main__":
    main()
