# aria2c 使用指南

## 基本用法

```
aria2c [选项] <URI|磁力链接|种子文件|Metalink 文件> ...
```

aria2 是一个命令行下载工具，支持 HTTP(S)/FTP/SFTP/BitTorrent/Metalink，可多源并发下载。

## 配置文件

默认路径：`~/.aria2/aria2.conf` 或 `$XDG_CONFIG_HOME/aria2/aria2.conf`

格式：每行 `NAME=VALUE`，`#` 开头为注释。

```ini
# 示例
continue=true
max-concurrent-downloads=5
```

---

## 核心选项

### 基本选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `-d, --dir=<DIR>` | 下载文件存储目录 | |
| `-o, --out=<FILE>` | 输出文件名（仅命令行 URI 有效） | |
| `-i, --input-file=<FILE>` | 从文件读取 URI 列表 | |
| `-l, --log=<LOG>` | 日志文件路径 | |
| `-j, --max-concurrent-downloads=<N>` | 最大并行下载数 | `5` |
| `-s, --split=<N>` | 每个文件使用 N 个连接下载 | `5` |
| `-x, --max-connection-per-server=<NUM>` | 每服务器最大连接数 | `1` |
| `-k, --min-split-size=<SIZE>` | 最小分片大小（影响连接数） | `20M` |
| `-c, --continue` | 续传已部分下载的文件 | `false` |
| `-V, --check-integrity` | 校验文件完整性 | `false` |
| `-h, --help` | 查看帮助（`--help=#http` 分类查看） | |
| `-v, --version` | 版本信息 | |

### 下载限速

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `--max-overall-download-limit=<SPEED>` | 全局最大下载速度（0=不限） | `0` |
| `--max-download-limit=<SPEED>` | 单任务最大下载速度（0=不限） | `0` |
| `--max-overall-upload-limit=<SPEED>` | 全局最大上传速度（0=不限） | `0` |
| `-u, --max-upload-limit=<SPEED>` | 单任务最大上传速度（0=不限） | `0` |
| `--lowest-speed-limit=<SPEED>` | 低于此速度则关闭连接（0=不限） | `0` |

速度值可附加 `K` / `M`（如 `500K`、`10M`）。

### 连接与重试

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `-t, --timeout=<SEC>` | 超时秒数 | `60` |
| `--connect-timeout=<SEC>` | 连接超时 | `60` |
| `-m, --max-tries=<N>` | 最大重试次数（0=无限） | `5` |
| `--retry-wait=<SEC>` | 重试间隔秒数 | `0` |
| `--max-connection-per-server=<NUM>` | 每服务器最大连接数 | `1` |

### HTTP 选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `--http-user=<USER>` | HTTP 用户名 | |
| `--http-passwd=<PASSWD>` | HTTP 密码 | |
| `--user-agent=<UA>` | 自定义 User-Agent | `aria2/$VERSION` |
| `--referer=<REFERER>` | 自定义 Referer | |
| `--header=<HEADER>` | 追加自定义请求头 | |
| `--load-cookies=<FILE>` | 从文件加载 Cookie | |
| `--save-cookies=<FILE>` | 保存 Cookie 到文件 | |
| `--check-certificate` | 验证 SSL 证书 | `true` |
| `--ca-certificate=<FILE>` | CA 证书文件（PEM） | |

### 代理选项

| 选项 | 说明 |
|------|------|
| `--all-proxy=<PROXY>` | 全局代理，格式 `[http://][USER:PASS@]HOST[:PORT]` |
| `--http-proxy=<PROXY>` | HTTP 代理 |
| `--https-proxy=<PROXY>` | HTTPS 代理 |
| `--no-proxy=<DOMAINS>` | 不走代理的域名列表（逗号分隔） |

### FTP/SFTP 选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `--ftp-user=<USER>` | FTP 用户名 | `anonymous` |
| `--ftp-passwd=<PASSWD>` | FTP 密码 | |
| `-p, --ftp-pasv` | 被动模式 | `true` |
| `--ftp-type=<TYPE>` | 传输类型：`binary` / `ascii` | `binary` |

### BitTorrent 选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `-T, --torrent-file=<FILE>` | .torrent 文件路径 | |
| `--listen-port=<PORT>` | TCP 监听端口 | `6881-6999` |
| `--enable-dht` | 启用 DHT | `true` |
| `--enable-peer-exchange` | 启用 PEX | `true` |
| `--bt-tracker=<URI>` | 附加 Tracker URI | |
| `--bt-exclude-tracker=<URI>` | 排除 Tracker URI | |
| `--bt-max-peers=<NUM>` | 每种子最大对等连接数 | `55` |
| `--seed-ratio=<RATIO>` | 分享率达到后停止做种 | `1.0` |
| `--seed-time=<MINUTES>` | 做种时间（分钟后停止） | |
| `--bt-stop-timeout=<SEC>` | 连续无速度 SEC 秒后停止（0=不限） | `0` |
| `--dht-listen-port=<PORT>` | DHT/UDP 监听端口 | `6881-6999` |
| `--dht-entry-point=<HOST>:<PORT>` | DHT 入口节点 | |

### Metalink 选项

| 选项 | 说明 |
|------|------|
| `-M, --metalink-file=<FILE>` | .meta4/.metalink 文件路径 |
| `--metalink-language=<LANG>` | 文件语言 |
| `--metalink-location=<LOC>` | 首选服务器地区（如 `jp,us`） |
| `--metalink-version=<VER>` | 文件版本 |

### RPC 选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `--enable-rpc` | 启用 JSON-RPC/XML-RPC 服务器 | `false` |
| `--rpc-listen-port=<PORT>` | RPC 监听端口 | `6800` |
| `--rpc-listen-all` | 监听所有网络接口 | `false` |
| `--rpc-secret=<TOKEN>` | RPC 授权令牌（推荐） | |
| `--rpc-secure` | 启用 SSL/TLS 加密 | `false` |
| `--rpc-certificate=<FILE>` | RPC 证书 | |
| `--rpc-private-key=<FILE>` | RPC 私钥 | |

### 系统与文件选项

| 选项 | 说明 | 默认值 |
|------|------|--------|
| `-D, --daemon` | 后台运行 | `false` |
| `-q, --quiet` | 静默模式 | `false` |
| `--stop=<SEC>` | SEC 秒后自动退出（0=不限） | `0` |
| `--disk-cache=<SIZE>` | 磁盘缓存大小 | `16M` |
| `--enable-color` | 彩色终端输出 | `true` |
| `--console-log-level=<LEVEL>` | 控制台日志级别（debug/info/notice/warn/error） | `notice` |
| `--save-session=<FILE>` | 退出时保存未完成任务 | |
| `--save-session-interval=<SEC>` | 自动保存会话间隔 | `0` |
| `--auto-save-interval=<SEC>` | 自动保存控制文件间隔 | `60` |
| `--allow-overwrite` | 覆盖已存在文件 | `false` |
| `--auto-file-renaming` | 重名文件自动重命名 | `true` |
| `--file-allocation=<METHOD>` | 文件分配方法（none/prealloc/trunc/falloc） | `prealloc` |
| `--human-readable` | 人性化显示文件大小 | `true` |
| `--remove-control-file` | 下载前移除 .aria2 控制文件 | |
| `--conf-path=<PATH>` | 指定配置文件路径 | |
| `--no-conf` | 禁用加载配置文件 | |

---

## 常用示例

**下载单个文件：**

```bash
aria2c https://example.com/file.zip
```

**指定输出文件名和目录：**

```bash
aria2c -d /downloads -o myfile.zip https://example.com/file.zip
```

**多连接加速（每服务器 4 连接 + 分 4 段）：**

```bash
aria2c -x 4 -s 4 https://example.com/large-file.zip
```

**断点续传 + 校验：**

```bash
aria2c -c -V https://example.com/file.zip
```

**限速下载：**

```bash
aria2c --max-download-limit=500K https://example.com/file.zip
```

**通过代理下载：**

```bash
aria2c --all-proxy=http://127.0.0.1:7897 https://example.com/file.zip
```

**多个 URI 下载同一文件（多源）：**

```bash
aria2c https://mirror1/file.zip https://mirror2/file.zip
```

**从文件读取 URI 列表：**

```bash
aria2c -i uris.txt
```

**下载 BT 种子：**

```bash
aria2c --seed-ratio=0.0 ubuntu-24.04-desktop-amd64.iso.torrent
```

（`--seed-ratio=0.0` 表示下载完成后不做种，直接退出）

**磁力链接：**

```bash
aria2c 'magnet:?xt=urn:btih:...'
```

**启用 RPC（配合 WebUI 使用）：**

```bash
aria2c --enable-rpc --rpc-listen-all --rpc-secret=mysecret -D
```

**后台下载：**

```bash
aria2c -D https://example.com/big-file.iso
```

---

## 退出状态码

| 码 | 含义 |
|----|------|
| 0 | 全部成功 |
| 1 | 未知错误 |
| 2 | 超时 |
| 3 | 资源未找到 |
| 5 | 速度过慢中止 |
| 6 | 网络问题 |
| 7 | 有未完成下载 |
| 9 | 磁盘空间不足 |

---

## 环境变量

- `http_proxy`、`https_proxy`、`ftp_proxy`、`all_proxy` — 代理设置
- `no_proxy` — 不走代理的地址列表
