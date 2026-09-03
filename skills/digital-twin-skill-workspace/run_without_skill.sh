#!/bin/bash
# 运行 without-skill 测试（无 skill，仅任务）
# 参数: $1 = eval prompt, $2 = output dir
hermes chat --provider my-newapi --model deepseek/deepseek-v4-flash \
  -q "$1\n\n完成后把最终答复保存到 $2/output.txt" \
  -Q 2>/dev/null | tee "$2/output.txt"