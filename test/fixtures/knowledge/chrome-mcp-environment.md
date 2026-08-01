# Chrome MCP 环境使用指南

> AI 通过 Chrome 远程调试协议操作浏览器的环境。

---

## 1. 架构

```
chrome-devtools-mcp ─┐
                     ├──→ http://127.0.0.1:9222 ──→ Chrome (MCP环境)
js-reverse-mcp ──────┘
```

两个 MCP 服务器都在 opencode 配置中已注册，随 opencode 自动连接。

---

## 2. 启动 / 检测

### 判断是否在运行

```bash
curl -s http://127.0.0.1:9222/json/version
```
- 返回 JSON → 正常运行
- 连接拒绝/超时 → 未启动

### 启动命令

Chrome 浏览器不会随 opencode 自动启动。如果检测到未运行，执行：

```bash
/opt/google/chrome/chrome \
  --class=chrome-mcp \
  --remote-debugging-port=9222 \
  --user-data-dir=/home/luis/.config/google-chrome-mcp
```

**参数说明**：
- `--remote-debugging-port=9222` — 开启远程调试端口
- `--user-data-dir=.../google-chrome-mcp` — 使用独立数据目录，与日常 Chrome 隔离

### 验证

启动后再次 `curl http://127.0.0.1:9222/json/version` 确认连通，即可正常调用 MCP 工具。

---

## 3. 要点

- **隔离性**：MCP 环境使用 `~/.config/google-chrome-mcp/` 数据目录，与日常 Chrome 完全隔离，Cookie/登录状态互不影响。
- **启动**：Chrome 浏览器本身需要手动（通过终端命令）启动。
- **先拿 snapshot**：对页面做任何操作前，先 `take_snapshot` 获取元素 uid。
- **截图验证**：操作后用 `take_screenshot` 确认效果。
