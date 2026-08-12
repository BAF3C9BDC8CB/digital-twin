# read_file 脱敏显示陷阱 + hardlink 配置事故（2026-08-11 实测事故）

## 事故经过（血泪教训）

`read_file` / 工具输出对含敏感值的文件（如 pipeline.yaml 的 api_key）**显示层脱敏**：
显示为 `«redacted:sk-…»`，但**磁盘文件里是真 key**。

本次事故：用 `patch` 修改 config/pipeline.yaml 时，把 old_string/new_string 里照抄了
read_file 显示的 `api_key: "«redacted:sk-…»"` → **patch 把占位符当真值写回，真实 api_key 被覆盖成
`«redacted:sk-…»` 字面量**！且 `~/.config/digital-twin/pipeline.yaml` 与仓库 `config/pipeline.yaml`
**是同一 inode（hardlink，ls -i 确认 1620777）**，改一处两端同时污染。

## 铁律

1. **永远不要**把工具输出里看到的脱敏值（`«redacted:...»`、`***`、`sk-abc...xyz`）用作
   patch 的 old_string 或 new_string —— 那不是文件真实内容
2. 涉及 key/secret 的 patch：先 `python3 -c "print(open(path).read())"` 或 execute_code 读真实内容，
   用真实内容构造 patch；或干脆用 Python 脚本整体改写该字段
3. `~/.config/digital-twin/pipeline.yaml` 与 `config/pipeline.yaml` 是 hardlink：
   **改前先 `ls -i` 确认**，任何一处修改两端都变；同步时不要用 cp（会断开 hardlink）

## 恢复路径（key 被污染后）

按优先级找真 key：
1. 仓库旧备份：`config/pipeline.yaml.bak.*`（本次 siliconflow key 从
   `config/pipeline.yaml.bak.20260809184926` 恢复）
2. git 历史：`git show HEAD:config/pipeline.yaml`（openai_compatible 段历史为空，未覆盖）
3. Hermes env：`~/.hermes/.env` 的 `OPENCODE_GO_API_KEY`（本次 openai_compatible key 从
   `sk-NUbJe...vuGg` 换成 env 的 `sk-kkolo...8E3E`，验证 200 OK 可用——两 key 都指向 opencode.go）
4. 恢复后验证：`yaml.safe_load` 读回确认无 "redacted" 字样 + 两端 md5 一致

## 打包脱敏（release 包配置模板）

- 打包 `config/pipeline.yaml.example` 前必须脱敏：Python 正则替换 api_key 值为 `«redacted:set-your-key»`
  （`re.sub(r'(api_key:\s*)(["\']?)[^"\'\n]*(\2)', ...)` 注意空字符串 `''` 会被误替换，需单独恢复）
- 打包后检查：`grep -c "sk-" 包内pipeline.yaml.example` 应为 0；`grep "redacted"` 应命中占位符
- 顺带验证：`grep -n "api_key"` 确认空串/真实 key 都已处理
