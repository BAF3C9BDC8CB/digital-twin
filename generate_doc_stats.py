#!/usr/bin/env python3
"""
生成 Hermes Agent 文档统计信息
"""
from pathlib import Path
import os

def count_file_stats(file_path):
    """统计单个文件的信息"""
    try:
        with open(file_path, 'r', encoding='utf-8') as f:
            content = f.read()
            lines = content.count('\n')
            chars = len(content)
            words = len(content.split())
        return lines, chars, words
    except:
        return 0, 0, 0

def main():
    """生成统计报告"""
    docs_dir = Path('hermes-docs-zh')
    
    if not docs_dir.exists():
        print("错误: hermes-docs-zh 目录不存在")
        return
    
    # 按目录分组统计
    stats_by_dir = {}
    total_files = 0
    total_lines = 0
    total_chars = 0
    total_words = 0
    
    for md_file in sorted(docs_dir.rglob('*.md')):
        if md_file.name == 'README.md':
            continue
            
        rel_path = md_file.relative_to(docs_dir)
        category = str(rel_path.parent) if rel_path.parent != Path('.') else '根目录'
        
        lines, chars, words = count_file_stats(md_file)
        
        if category not in stats_by_dir:
            stats_by_dir[category] = {
                'files': [],
                'total_lines': 0,
                'total_chars': 0,
                'total_words': 0
            }
        
        stats_by_dir[category]['files'].append({
            'name': md_file.name,
            'lines': lines,
            'chars': chars,
            'words': words
        })
        stats_by_dir[category]['total_lines'] += lines
        stats_by_dir[category]['total_chars'] += chars
        stats_by_dir[category]['total_words'] += words
        
        total_files += 1
        total_lines += lines
        total_chars += chars
        total_words += words
    
    # 输出统计报告
    print("=" * 80)
    print("Hermes Agent 中文文档统计报告")
    print("=" * 80)
    print()
    
    for category in sorted(stats_by_dir.keys()):
        info = stats_by_dir[category]
        print(f"📁 {category}")
        print(f"   文件数: {len(info['files'])}")
        print(f"   总行数: {info['total_lines']:,}")
        print(f"   总字符: {info['total_chars']:,}")
        print(f"   总词数: {info['total_words']:,}")
        print()
        
        # 显示该目录下文件列表
        for file_info in sorted(info['files'], key=lambda x: x['lines'], reverse=True):
            print(f"      - {file_info['name']:<40} {file_info['lines']:>6} 行")
        print()
    
    print("=" * 80)
    print("📊 总体统计")
    print("=" * 80)
    print(f"总文件数:   {total_files}")
    print(f"总行数:     {total_lines:,}")
    print(f"总字符数:   {total_chars:,}")
    print(f"总词数:     {total_words:,}")
    print(f"平均行数:   {total_lines // total_files if total_files > 0 else 0}")
    print(f"总大小:     {total_chars / 1024:.2f} KB")
    print()

if __name__ == "__main__":
    main()
