# 日志管线(2026-08-13):异步写入 + 全命令覆盖 + 日期/大小双维度轮转

## 概览
- 统一入口 `src/shared/logging/init.rs::init_logging()` → 返回 `LogGuard`。
- 文件层:JSON 结构化(flatten_event),经 **RotatingWriter 轮转**写 `$DT_LOG_DIR`。
- stderr 层:人类可读 compact 格式,默认仅 WARN+(DT_LOG_STDERR 可调)。
- **异步**:文件层与 stderr 层均经 `tracing_appender::non_blocking`,事件入队列即返回,worker 落盘。
- `LogGuard` 必须存活到进程退出(drop 冲刷队列):main.rs 与 mcp.rs 均 `let _log_guard = init_logging()?;`。

## 轮转(RotatingWriter,src/shared/logging/rotating.rs)
- **日期维度**:每天一个 `dt.yyyy-MM-dd.log`(本地时区,午夜切换)。
- **大小维度**:单文件超阈值切同日序号 `dt.yyyy-MM-dd.1.log`、`.2.log`…。
- **保留**:目录内日志文件总数超限删最旧;`dt.log` 软链恒指向当前写入文件。
- **旧文件迁移**:首次启用时普通文件 dt.log 自动归档 `dt.legacy-<时间戳>.log`(一次性)。
- env:
  | 变量 | 默认 | 说明 |
  |------|------|------|
  | `DT_LOG_DIR` | /var/log/digital-twin | 日志目录(不可写回退 /tmp) |
  | `DT_LOG_MAX_BYTES` | 52428800 (50MiB) | 单文件大小阈值 |
  | `DT_LOG_RETENTION_FILES` | 30 | 保留文件总数上限 |
  | `DT_LOG_LEVEL` / `RUST_LOG` | info | 日志级别(debug 开详细) |
  | `DT_LOG_STDERR` | warn | stderr 层级别 |

## ⚠️ 关键坑(实测踩过)
1. **重启续写必须取"最大序号"而非"从 0 探测"**:保留清理会删中间序号形成空洞(如只剩 .38/.39),从 0 探测第一个不存在的序号会错误地重新从 .0 写起。`open_current` 用 `max_seq_for_date()` 扫描目录取最大序号。回归测试 `restart_continues_max_seq_across_holes` 覆盖。
2. 一次 dt health 有 ~13 个 info 事件(含 neo4rs 等三方库);小阈值(如 300B)测试时一次命令就切十几个文件,可能触发保留清理——验证脚本断言要按此设计。
3. 软链刷新失败仅 warn(不致命);rotate 时旧文件 `sync_all` 尽力落盘。
4. 多进程(CLI + dt-mcp 同写)依赖 O_APPEND 单次 write 原子性;序号竞争只可能导致多切文件,不损坏数据。
5. `RotatingWriter::new` 里 migrate_legacy 用 eprintln(此时 subscriber 未 init,tracing 无效)。

## 等级设计(默认 info)
- info:命令入口/完成/结果摘要;debug:内部细节;warn:异常/降级/破坏性未确认。
- **dt sense 特殊**:入口 debug(高频,Hermes 每轮调用不刷日志),结果 info + 降级 warn。

## 全命令日志覆盖(2026-08-13)
- build/search/memorize/learn/event/backup(list/verify/create)/cleanup(schema/clean/health)/sense 全部有日志。
- handle_search 补"搜索完成"info(含 total/per_world/degraded)。
- dt-mcp(mcp.rs)复用 init_logging(原自建同步 stderr subscriber 已移除)——MCP 走 stdin/stdout,日志落 dt.log 不污染协议流。

## 陷阱
- tracing fmt layer writer 必须显式 stderr(默认 stdout)——U-D4 stdout 纯净。
- guard 提前 drop 丢日志;cargo build --release 后软链 ~/.local/bin/dt 自动生效。
- 测试:652 全过;cargo test 看 'test result' 勿 tail -1。
