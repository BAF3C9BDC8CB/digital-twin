# dt 部署方式 = 软链接（2026-08-11 确认）

`~/.local/bin/dt -> /data/myProject/digital-twin-v2/target/release/dt`（软链接，不是拷贝；/usr/local/bin 下没有 dt）。

**含义**：
- `cargo build --release` 完成后新二进制**立即生效**，无需任何安装步骤。
- 用户说「打包 release 即可，有软链接不需要安装」= 打包任务 = build + tar 组装两步，
  **跳过 install 到 /usr/local/bin**（README 里的 `sudo install` 只对无软链接部署场景有意义）。
- 验证当前生效二进制：`ls -la $(which dt)` 看软链接指向；`readlink -f $(which dt)` 得真实路径。

与 release-packaging.md 配套：打包流程细节（脱敏、顺序坑、SHA256）见 `references/release-packaging.md`。
