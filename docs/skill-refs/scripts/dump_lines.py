#!/usr/bin/env python3
"""Dump a line range of a file with line numbers.

读含中文的 .rs 文件时 read_file 会误报 binary,用本脚本替代:
    python3 dump_lines.py <path> <start> <end>

也解决了在 execute_code 里用 f-string 嵌套生成 python -c 代码时
大括号 {i:4d} 被外层 f-string 误解析的转义问题。
"""
import sys

path, start, end = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
lines = open(path, encoding='utf-8').read().splitlines()
for i in range(start, end + 1):
    print(str(i).rjust(4) + '|' + lines[i - 1])
