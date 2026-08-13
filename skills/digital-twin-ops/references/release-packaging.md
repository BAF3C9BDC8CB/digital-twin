# digital-twin release 打包流程（2026-08-11 实测）

用户说「打包正式 --release 包」时的标准流程。仓库无打包脚本/无 [profile.release] 自定义配置，
全项目索引构建 = `dt build`（66 个项目逐个执行，无独立脚本，build-all.sh 已删），**不是二进制打包**——别混淆。

## 步骤

```bash
cd /data/myProject/digital-twin-v2
git log -1 --format="%h %s"        # 记录对应 commit（未 commit 的改动会打进包，需向用户说明）
cargo build --release              # 产出 target/release/dt（~34MB，digital-twin 0.1.0）
./target/release/dt --version      # 确认版本

# 组装发布目录（内容清单是 2026-08-11 拍板的标准集）
RELEASE_DIR=/tmp/dt-release-v0.1.0
mkdir -p $RELEASE_DIR/{bin,mcp,config/prompts,docs}
cp target/release/dt $RELEASE_DIR/bin/
cp mcp/mcp-server.py $RELEASE_DIR/mcp/
cp config/config.yaml.example $RELEASE_DIR/config/config.yaml.example
cp config/pipeline.yaml $RELEASE_DIR/config/pipeline.yaml.example   # ⚠️ 改名 .example，别带真实 key
cp config/event-hooks.yaml $RELEASE_DIR/config/event-hooks.yaml.example  # ⚠️ 源文件无 .example 后缀，别用 event-hooks.yaml.example
cp config/prompts/*.yaml $RELEASE_DIR/config/prompts/
cp docs/phase2-self-healing-spec.md $RELEASE_DIR/docs/ 2>/dev/null  # 本次架构文档，可选

# README 必须含：安装路径（config.yaml/pipeline.yaml 固定 ~/.config/digital-twin/）、
# 快速验证命令（dt --version / dt health / dt build / dt search）、依赖后端清单、
# 本版本关键特性、--no-llm-backfill 开关说明

tar czf /tmp/dt-release-v0.1.0.tar.gz -C /tmp dt-release-v0.1.0/
sha256sum /tmp/dt-release-v0.1.0.tar.gz    # 报给用户

# 冒烟测试：解包后直接跑二进制（tar 解包目标目录必须先 mkdir -p，tar 不会自建）
mkdir -p /tmp/dt-release-smoke && tar xzf dt-release-v0.1.0.tar.gz -C /tmp/dt-release-smoke
/tmp/dt-release-smoke/dt-release-v0.1.0/bin/dt --version && .../dt --help | head -3
```

## 关键坑

- **发布包二进制 = 工作区当前代码**，与 git commit 不一定对应。若用户要求包与提交对应，
  先 commit 再重打包（本仓库惯例不主动 commit，需用户确认）。
- pipeline.yaml 必须存 `.example` 名，**绝不带真实 API key**。
- **脱敏实现细节（2026-08-11 实测踩坑，含 v0.1.1 二次修正）**：直接 `cp config/pipeline.yaml` 会把真实 key 打进包。
  脱敏时注意——① 逐行正则 `api_key:\s*["']?[^"'\n]+["']?` 会把 `api_key: ''` 空值行也匹配，
  且引号边界处理不当会污染成 `"«redacted:set-your-key»"'`（多余引号）；② 正确做法：先判断行
  是否匹配 `api_key: "..."` 带引号值的形态，空串行（`''`）保留原样；③ 完成后验证
  `grep -c "sk-" <包内文件>` 应为 0，且 `grep -c redacted` 数量与预期 api_key 行数一致。
- ⚠️ **无引号 api_key 形态 + 显示层截断（v0.1.1 实测）**：pipeline.yaml 的 api_key 值可能**不带引号**
  （`api_key: sk-xxx...`）。① 带引号正则对此 0 匹配 → 真实 key 原样进包（grep "sk-" 会命中但
  容易被忽略）；② Hermes 工具输出（read_file/execute_code 结果）会把 sk- 开头值**截断显示**成
  `sk-iey...koip` 形态——看着像省略号，**磁盘上其实是完整 key**，别据此判断"已脱敏"。
  正确脱敏脚本：用 Python 逐行正则 `^(\s*api_key:\s*)(?:"([^"]*)"|'([^']*)'|([^\s#].*?))\s*$`
  覆盖带双引号/单引号/无引号三种形态，空值行（`''`）保留；完成后**字节级验证**：
  ```python
  raw = open(path, encoding="utf-8").read()
  leak = [l for l in raw.splitlines() if 'sk-' in l]          # 必须为 0
  red  = [l for l in raw.splitlines() if 'redacted' in l]     # 数量 = 脱敏的 key 数
  # 逐行 split('api_key:',1)[1].strip().strip('"\'') 检查 len(val)==0 或含 redacted
  ```
  打包后 `tar xzf ... && grep -rc "sk-" <包内目录>/ | grep -v ":0"` 应为空。
- 配置装载约定（写进 README）：`config.yaml` 读固定 `~/.config/digital-twin/config.yaml`；
  `pipeline.yaml` 读固定 `~/.config/digital-twin/pipeline.yaml`（2026-08-06 修复后），
  prompts 目录也在 `~/.config/digital-twin/prompts`。
- `dt --version` 输出 `digital-twin 0.1.0`（package name 是 digital-twin，bin 名 dt），别写成 dt 0.1.0。
- 打包内容若含 docs/，注意当前工作区 docs 下可能有未提交的设计文档（本次的
  phase2-self-healing-spec.md），属合理随包内容。
- 打包前 `pgrep -af "dt build"` 确认无残留构建进程（旧二进制构建会干扰验证）。

## 版本号策略（现状）

Cargo.toml 固定 `version = "0.1.0"`，无 git tag 流程、无 CHANGELOG。打包文件名用手工版本
（dt-release-v0.1.0.tar.gz / v0.1.1 / v0.1.2）。若未来做正式发版，需先引入版本管理（用户未要求，勿自作主张）。

## v0.1.2 实测补充（2026-08-11）

- **仓库只有 `config/config.yaml.example` 一个 example**；pipeline/event-hooks 的 example 需现做：
  本次用 write_file 手写 `pipeline.yaml.example`（含 max_tokens 字段 + 失败重试说明注释，
  api_key 用 `sk-YOUR_SILICONFLOW_KEY` / `sk-YOUR_OPENAI_COMPATIBLE_KEY` 占位），
  event-hooks 直接 `cp config/event-hooks.yaml`（无敏感 key，可原样改名）。
- **⚠️ 顺序坑（本次实际发生）**：先 write_file 写 `$REL/config/pipeline.yaml.example`，
  再跑 `rm -rf $REL && mkdir -p ...` 组装命令 → 已写的文件被 rm 清掉，还得重写。
  **正确顺序：先 `rm -rf $REL && mkdir -p` 建目录，再 write_file/cp 灌文件。**
- README.md 手工写（write_file），内容含：包内容清单、安装步骤、本版本更新点
  （v0.1.2 = LLM 失败重试机制；v0.1.1 = max_tokens/UA/backfill 并发/ensure_collection/ignore_files/clear_all）、
  常用命令、注意事项（⚠️ 构建期间不要并行跑多个 dt 命令）。
- 打包后验证三步：`find . -type f` 核对清单 → `grep -rn "sk-" | grep -v YOUR_` 确认无真实 key →
  解包冒烟 `bin/dt --version`（v0.1.2 包实测 SHA256 7945453db6460426e02fa8f5595d09a7018464fa6df186ab926375a0a15d703d，10.5MB）。
- `dt --version` 输出始终是 `digital-twin 0.1.0`（Cargo.toml 版本），**不随 tar 文件名变**——用户问版本时
  报 tar 名（v0.1.2）+ SHA256，别报二进制里的 0.1.0 混淆。

## v0.1.3 双平台打包（2026-08-13 实测，Windows 支持首版）

- **Windows 交叉编译**: 本机已有 `rustup target x86_64-pc-windows-gnu` + `/usr/bin/x86_64-w64-mingw32-gcc`。
  命令: `cargo build --release --target x86_64-pc-windows-gnu`（产出 dt.exe/dt-mcp.exe，约 39MB，PE32+ console, x86-64）。
  rusqlite bundled / tree-sitter C 代码用 mingw gcc 编译无问题。
- **Windows 兼容改动（本次 7 文件）**:
  ① `src/shared/mod.rs` 新增 `home_dir()`（HOME → USERPROFILE → HOMEDRIVE+HOMEPATH），替换 5 处
  `std::env::var("HOME")`（runtime.rs dirs_like_home_config、cli/build.rs project_roots_from_config、
  build/pipeline.rs load_code_analysis_prompt、pipeline/prompt.rs、pipeline/config.rs home_pipeline_config）
  ——Windows 无 HOME 变量，不改则配置加载失败。
  ② `src/mcp.rs` capture_stdout 唯一 unix-only 点（dup/dup2 + AsRawFd）：cfg(unix) 保留原实现，
  cfg(windows) 用 `libc::open_osfhandle(handle, 0)` 把 File HANDLE 转 CRT fd 再 dup2
  （libc 在 windows target 函数名**无下划线**：open_osfhandle/close 而非 _open_osfhandle/_close），
  close(fd) 后 `std::mem::forget(file)` 防二次 CloseHandle；临时文件从 `/tmp/` 改 `std::env::temp_dir()`。
- **包结构（双平台）**: `bin/{dt,dt-mcp}` + `bin/windows/{dt.exe,dt-mcp.exe}`；其余同 v0.1.2。
- **文档**: 完整配置文档 `docs/CONFIG.md`（包内 + 项目 docs/ 各一份），含 Windows 专章
  （Docker Desktop 跑 Memgraph/Qdrant + 原生 exe；或 WSL2 全 Linux；%USERPROFILE%\.config\digital-twin\
  路径；MCP 客户端 json 示例；防火墙/SmartScreen/UTF-8 chcp 65001 注意事项）。
- **脱敏验证升级**: 全包文本 grep "sk-" 零命中——pipeline.yaml.example 占位符用
  `REPLACE_WITH_YOUR_KEY`（不带 sk- 前缀，v0.1.2 的 sk-YOUR_XXX_KEY 会命中 grep）；CONFIG.md 示例同。
  验证脚本: python 遍历 tar 内所有 .yaml/.md 逐行查 sk-。
- 归档: 正式包 `dist/dt-release-v0.1.3.tar.gz`（项目内），SHA256 749eda04272b104e5cb9b0b75eeb795cf8036bc0c9b69f9a929c7a264b0466c9。
- `dt --version` 实际输出 `dt 0.1.0`（bin 名 dt；v0.1.2 记录 digital-twin 0.1.0 有出入，以实际为准）。

## v0.1.3 二次修正（2026-08-13，验证抓到真实 bug）

- **HOMEDRIVE+HOMEPATH 分支的 join 坑**: `PathBuf::from("D:").join("\\Users\\x")` 在 Windows 上
  join 把以 `\` 开头的 HOMEPATH 当 root-relative 路径 → 结果丢盘符变成 `\Users\x`。
  修复: 字符串拼接 `format!("{}{}", drive, path)`（shared::home_dir）。
- **验证方法（临时集成测试）**: 在 tests/ 写 `hermes_verify_*.rs` 直接测 shared::home_dir()
  四种场景（HOME/USERPROFILE/HOMEDRIVE+HOMEPATH/全缺省），跑 `cargo test --test <name>` 后删除。
  比黑盒 dt sense 验证更直接（sense 的 project 识别依赖后端在线, 环境变量组合不可控）。
- **env::set_var 在新 rustc 需 unsafe 块**（edition 2021 也报, 直接包 unsafe 即可）。
- v0.1.3 最终 SHA256: 531ba64608720066ae868f4be20d26b17c3572c8b52dfc42556ca5281128eda1。
