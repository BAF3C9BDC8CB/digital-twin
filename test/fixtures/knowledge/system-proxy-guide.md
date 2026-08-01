# 系统代理使用指南

## 代理信息

| 项目 | 值 |
|------|-----|
| 类型 | HTTP / HTTPS / SOCKS5 |
| 地址 | `127.0.0.1` |
| 端口 | `7897` |
| 工具 | Clash |

## 推荐方式：全局设置，全部走代理

在 shell 配置文件中添加（`~/.bashrc` 或 `~/.zshrc`）：

```bash
export http_proxy=http://127.0.0.1:7897
export https_proxy=http://127.0.0.1:7897
export all_proxy=socks5://127.0.0.1:7897
```

生效后**所有流量都走代理**，包括 `curl`、`npm`、`pip`、`git`、`docker` 等，无需逐个工具单独配置。

```bash
source ~/.zshrc   # 立即生效
```

## 不走代理的地址

内网和本地地址不需要走代理，配合 `NO_PROXY` 排除：

```bash
export NO_PROXY=localhost,127.0.0.1,.local,.internal,10.0.0.0/8,192.168.0.0/16
```

建议将上面两段（`http_proxy` + `NO_PROXY`）一起加到 shell 配置文件中。

## 验证代理是否正常

```bash
curl -x http://127.0.0.1:7897 -I https://www.google.com
# 返回 200 表示正常；Connection refused 表示 Clash 未运行
```

## 注意

- Clash 必须保持运行，否则设置了代理也会连接失败
- `sudo` 会清除环境变量，需用 `sudo -E` 保留代理设置
