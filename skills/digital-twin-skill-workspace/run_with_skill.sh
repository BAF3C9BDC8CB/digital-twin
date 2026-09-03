#!/bin/bash
# 运行 with-skill 测试（注入优化后的 digital-twin-skill 内容）
# 参数: $1 = eval prompt, $2 = output dir
SKILL="$(cat /data/myProject/digital-twin-v2/skills/digital-twin-skill/SKILL.md)"
hermes chat --provider my-newapi --model deepseek/deepseek-v4-flash \
  -q "你被赋予以下 SKILL（知识图谱操作手册）。请按其规则完成任务。\n\n=== SKILL ===\n$SKILL\n\n=== 任务 ===\n$1\n\n完成后把最终答复保存到 $2/output.txt" \
  -Q 2>/dev/null | tee "$2/output.txt"