# OCR (OpenCode-Register) MCP 化状态与工具清单 (2026-08-07)

> 用户决策:不要 CLI 命令,只保留 MCP 工具,人工走网页。
> 方案终稿:`/data/doc/设计方案/opencode-register-cli-mcp-改造方案-FINAL.md`(v2 仅 MCP 版;v1 含 CLI 备份为 `-v1-full.md`)。

## 状态

- `backend/mcp_server.py`(1575 行,commit 411f3b8)已实现并注册进 Hermes:
  `hermes mcp list` → `ocr-mcp-server` 启用;`hermes mcp test ocr-mcp-server` → stdio 连接,
  25 tools discovered。
- 验收(k3 终审 2026-08-07):通过。25 工具全部注册、可加载、调 FastAPI 8888 + CDP 9223、
  不绕过 Vault、凭据默认掩码、错误码映射 423/409/404/503 齐全、零侵入(仅新增 mcp_server.py +
  pyproject 依赖)。
- 验收报告:`/data/doc/设计方案/ocr-mcp实现评审.md`。

## 架构

```
AI Agent (Hermes) → ocr-mcp-server (stdio MCP)
                     ├─ HTTP 8888 → FastAPI(账号/流程/Vault/设置,不绕过 Vault)
                     └─ CDP WS 9223 → CloakBrowser(DOM 读取/点击/截图/console)
人类用户 → 网页 http://127.0.0.1(扫码付款/GitHub 验证码/主密码/API Key 补救)
```

端口分工:9222 Chrome MCP / 9223 CloakBrowser(OCR-MCP 专用)/ 9333 Hermes 内置。

## 25 个 MCP 工具

- 账号管理:ocr_account_list / counts / show(show_* 显式才明文)/ ship
- 流程控制:ocr_flow_create / subscribe / qr / confirm / status / manual / cancel
- Vault:ocr_vault_status / unlock(读 ~/.config/opencode-register/.vault-master-password)
- 浏览器控制:ocr_browser_pages / navigate / read_dom / click / type / screenshot / wait / console / health
- 设置健康:ocr_health / settings_list / settings_update

关键实现点(验收时确认):
- `BACKEND_URL=http://127.0.0.1:8888`,`CDP_URL=http://127.0.0.1:9223`
- 后端调用统一走 `_backend_get/_backend_post/_backend_patch/_backend_put`;CDP 走 `CdpClient` 封装
- 凭据掩码 `_mask_api_key()`(sk-abc...xyz)/ `_mask_password()`(*** (N chars))/ `_mask_public_link()`,
  默认 show_*=False,显式才明文
- 错误映射 `_map_backend_response_error`:400 参数无效/401 认证失败/404 不存在/409 冲突/422 字段/
  423 vault_locked(提示先 unlock)/500/502/503

## 全生命周期流程(五阶段,S0-S5)

S0 前置(health+vault unlock)→ S1 创建(邮箱→GitHub→OAuth→pending_subscribe)→
S2 订阅(GeoMock 账单→支付宝→CDP 检测二维码→飞书)→ S3 支付(扫码→manual-input confirmed→
API Key 落库→active;支付后刷新 go 页确认已订阅)→ S4 中国区模型(CDP 勾选 useChinaProviders,
刷新验证 checked 持久化)→ S5 发货(读凭据→CAS 标记 sold→飞书交付,不含 GitHub 用户名)。

人工介入点(全部走网页/CloakBrowser 窗口):主密码、GitHub CAPTCHA、邀请码确认、
**支付宝扫码付款(唯一硬性)**、API Key 手动粘贴、CDP 手动登录。

## 与运营 skill 的关系

`opencode-register-account-ops` skill(devops/,v2.0)已覆盖运营流程(curl/API 版)。
本 reference 补充 MCP 化后的工具清单与验收状态。未来运营优先走 MCP 工具,
curl/CDP 脚本为兜底。该 skill 为 user-owned,如需 curator 维护:
`hermes curator adopt opencode-register-account-ops`。
