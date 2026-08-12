# config.yaml scanner 段死配置审计 + 64 项目噪音普查 (2026-08-10)

## 代码追踪证据

- `src/application/build/service.rs:67` — `BuildService::new()` 硬编码 `scan_config: ScanConfig::default()`
- `src/application/build/service.rs:75` — `with_scan_config()` 定义存在, 但 `rg with_scan_config src/` 显示**全库零调用**
- `src/main.rs` — `DaemonConfig` 只反序列化 `projects/services/batch` 等; `rg scanner src/main.rs` 无命中 → config.yaml 的 `scanner:` 段无结构体承接
- `src/domain/types.rs:362-410` — `ScanConfig` 字段: `ignore_dirs` / `ignore_ext` / `max_file_size` / `document_extensions` / `max_doc_file_size`。**无 `ignore_files` 字段**; 默认 ignore_dirs 13 项, ignore_ext 19 项
- `src/infrastructure/scanner.rs:20-27` (`collect_files`) 与 `:67-74` (`collect_document_files`) — `filter_entry` 对目录用 `entry.file_name()`(单段名)查 `ignore_dirs` HashSet; 多段路径条目永不匹配。文件级忽略仅硬编码三条: `.min.js` / `.bundle.js` / `.generated.`

## 实际生效的默认值 (ScanConfig::default)

```
ignore_dirs: node_modules .git target build __pycache__ .venv dist .next vendor .idea .vscode coverage .nyc_output
ignore_ext:  .class .jar .war .so .dll .exe .bin .png .jpg .jpeg .gif .svg .ico .zip .tar .gz .bz2 .pdf .lock
max_file_size: 524288 (500KB)
document_extensions: md txt pdf yaml yml properties
```

## 64 项目噪音普查 (遍历 config.yaml projects 全部条目)

统计方法: Python 遍历 `~/.config/digital-twin/config.yaml` 的 `projects[].items`(含 `{alias: rel}` 映射), 解析出 64 个存在项目根; 一级目录统计全部 items 的直系子目录, 二级目录统计每个项目根下所有一级子目录的直系子目录。

### 一级目录 (出现项目数, 均未被默认 ignore 覆盖)

| 目录 | 项目数 | 性质 |
|------|--------|------|
| charts | 36 | Helm 部署清单, 非源码 |
| public | 12 | 前端静态资源 (public/uploads, public/img, public/static) |
| docs | 10 | 文档目录 (用户可拍板保留) |
| .mvn | 10 | Maven wrapper |
| tests | 8 | 测试代码 (用户可拍板) |
| logs | 6 | 运行日志 |
| .github | 5 | CI 配置 |
| .weave | 3 | 工具运行时 |
| static | 2 | 静态资源 |
| .git | 59 | 已在默认 ignore |
| .idea | 28 | 已在默认 ignore |
| target | 26 | 已在默认 ignore |
| node_modules | 10 | 已在默认 ignore |

### 二级目录 (频次, 示例)

- target ×35 (已被默认 ignore 的 target 单段覆盖, 无需处理)
- assets ×18 (src/assets 等)
- static ×10 (public/static, uv-admin/static 等)
- runtime ×10 (几乎全在 .weave/runtime 下 → .weave 未忽略)
- libs ×3 (src/libs)
- build ×3 (yijianbao_shop_web/build, yingchao_web/build, hospital-hive/build)
- logs ×2 (business-center-server/logs, pay-offen-payment/logs)
- docs ×2 (yijianbao_shop/docs, yijianbao_shop_web/docs)
- node_modules ×2, __pycache__ ×2 (均在默认 ignore)
- img ×1 (public/img), .mvn ×1, doc ×1 (web/doc), uploads ×1 (web/uploads)

### 建议补充的 ignore_ext (默认仅 19 项, 缺失常见噪音)

`.pyc .pyo .log .iml .tsbuildinfo .sqlite .map` (+ 已有硬编码覆盖 .min.js/.bundle.js/.generated.)

## 修复方案 (2026-08-10 提出, 待用户确认)

Step 1 代码 (4 处):
1. `ScanConfig` 增加 `ignore_files: HashSet<String>` (types.rs)
2. main.rs `DaemonConfig` 增加 `scanner` 段解析 → 构造 `ScanConfig`
3. `BuildService` 构造时调用 `with_scan_config` (service.rs:67 替换死默认)
4. `scanner.rs` 目录匹配升级为「单段名 OR 相对路径前缀」; 文件按 `ignore_files` 名单匹配

Step 2 配置 (config.yaml 两端同步: `~/.config/digital-twin/config.yaml` + 仓库 `config/config.yaml`):
- ignore_dirs 补: `charts docs logs .mvn .weave static assets runtime libs uploads img doc`
- ignore_ext 补: `.pyc .pyo .log .iml .tsbuildinfo .sqlite .map`
- 待用户拍板: docs / tests 是否忽略; public / assets / static 是否全忽略; 改后是否重跑增量构建验证

验证门禁: `cargo check` + scanner 相关单测; 配置改动后跑一次增量构建确认 `files_scanned` 明显下降、无 429/502。
