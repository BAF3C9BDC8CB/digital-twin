#!/usr/bin/env python3
"""每日定时全项目增量构建（Hermes cron no_agent 启动器）。

用法:
  正常调度 (cron 每天 12:00): 本脚本作为启动器——
    1. 原子创建锁文件（O_EXCL），已运行则立即跳过（防重入）
    2. setsid 后台派生 worker 进程（脱离 cron 超时控制），立即返回
  后台 worker: 遍历 config.yaml 全部注册项目，逐项目执行
    `dt build --path <root> --name <name>`（增量：mtime+hash 只处理
    变更文件，无变更项目仅扫描+对比，秒级~几十秒）
    结果写入 /tmp/dt-daily-build.log，锁在结束时清理

设计理由（为什么不是 watcher）:
  不监听文件系统（65+ 项目 inotify 递归监听会撞 max_user_watches 上限
  且事件风暴频繁），每天一次增量构建作为手动修改文件的兜底拾取——
  与 dt-auto-build.py（Hermes 写文件即时触发）互补：
  - Hermes 改文件 → post_tool_call hook 即时增量（单文件/项目级）
  - 手动改文件 → 本定时任务每日 12:00 全项目增量兜底

测试:
  DT_DAILY_BUILD_DRY=1 python3 scripts/dt-daily-build.py   # dry-run worker
"""

import os
import subprocess
import sys
import time
from pathlib import Path

# ── 配置 ──────────────────────────────────────────────────────────────
DT_BIN = os.environ.get("DT_BIN", str(Path.home() / ".local/bin/dt"))
CONFIG_YAML = os.environ.get(
    "DT_CONFIG_YAML", "/data/myProject/digital-twin-v2/config/config.yaml"
)
LOG_FILE = Path(os.environ.get("DT_DAILY_LOG", "/tmp/dt-daily-build.log"))
LOCK_FILE = Path(os.environ.get("DT_DAILY_LOCK", "/tmp/dt-daily-build.lock"))
DRY = os.environ.get("DT_DAILY_BUILD_DRY") == "1"
PER_PROJECT_TIMEOUT = 1800  # 单项目构建超时（LLM backfill 慢项目可能数分钟）


def log(msg: str) -> None:
    try:
        LOG_FILE.parent.mkdir(parents=True, exist_ok=True)
        with open(LOG_FILE, "a") as f:
            f.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}\n")
    except Exception:
        pass


def load_projects() -> list:
    """从 config.yaml 解析 (项目名, 根路径) 列表（与 dt 的 resolve_project_paths 同构）。"""
    try:
        import yaml
    except ImportError:
        log("PyYAML 不可用，无法解析 config.yaml")
        return []
    try:
        cfg = yaml.safe_load(open(CONFIG_YAML))
    except Exception as e:
        log(f"config.yaml 读取失败: {e}")
        return []
    out = []
    for group in cfg.get("projects", []):
        base = Path(group.get("base", ""))
        for item in group.get("items", []):
            if isinstance(item, str):
                out.append((item, base / item))
            elif isinstance(item, dict):
                for k, v in item.items():
                    rel = str(v) if isinstance(v, str) else k
                    out.append((str(k), base / rel))
    return out


def run_builds() -> None:
    """worker：逐项目增量构建，汇总结果。"""
    start = time.time()
    projects = load_projects()
    if not projects:
        log("没有可构建的项目（config.yaml 解析为空）")
        return
    log(f"开始每日构建: 共 {len(projects)} 个项目")

    ok, fail = [], []
    for name, root in projects:
        if not root.exists():
            fail.append((name, "目录不存在"))
            log(f"[fail] {name} 目录不存在: {root}")
            continue
        if DRY:
            log(f"[dry] {name} @ {root}（模拟，不实际构建）")
            continue
        t0 = time.time()
        try:
            r = subprocess.run(
                [DT_BIN, "build", "--path", str(root), "--name", name],
                capture_output=True,
                text=True,
                timeout=PER_PROJECT_TIMEOUT,
            )
            dt = time.time() - t0
            if r.returncode == 0:
                ok.append(name)
                log(f"[ok] {name} {dt:.0f}s")
            else:
                err = (r.stderr or "").strip().splitlines()
                msg = err[-1][:200] if err else f"exit={r.returncode}"
                fail.append((name, f"exit={r.returncode}"))
                log(f"[fail] {name} {dt:.0f}s {msg}")
        except subprocess.TimeoutExpired:
            fail.append((name, "超时"))
            log(f"[fail] {name} 超过 {PER_PROJECT_TIMEOUT}s 超时")
        except Exception as e:
            fail.append((name, str(e)))
            log(f"[fail] {name} {e}")

    total_min = (time.time() - start) / 60
    summary = f"每日构建完成: 成功 {len(ok)}/{len(projects)} 失败 {len(fail)} 总耗时 {total_min:.1f} 分钟"
    log(summary)
    if fail:
        log("失败项目: " + ", ".join(f"{n}({r})" for n, r in fail))
    if DRY:
        log("（dry-run，未实际执行构建）")


def _lock_held() -> bool:
    """锁文件存在且对应 PID 存活 → 认为仍在运行；过期锁自动清除。"""
    try:
        pid = int(open(LOCK_FILE).read().strip())
    except Exception:
        return False
    if Path(f"/proc/{pid}").exists():
        return True
    try:
        LOCK_FILE.unlink()
    except OSError:
        pass
    return False


def main() -> None:
    role = os.environ.get("DT_DAILY_BUILD_ROLE", "")
    if role == "worker":
        try:
            run_builds()
        finally:
            try:
                LOCK_FILE.unlink()
            except OSError:
                pass
        return

    # ── 启动器角色：防重入 + 后台派生 worker，立即返回 ──
    if _lock_held():
        print("每日构建已在运行（锁存在且 PID 存活），本次跳过")
        return
    try:
        fd = os.open(LOCK_FILE, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        os.write(fd, str(os.getpid()).encode())
        os.close(fd)
    except FileExistsError:
        print("每日构建已在运行（锁竞争），本次跳过")
        return

    script = Path(__file__).resolve()
    env = dict(os.environ)
    env["DT_DAILY_BUILD_ROLE"] = "worker"
    subprocess.Popen(
        [sys.executable, str(script)],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,  # 脱离会话：cron 进程退出不影响 worker
        close_fds=True,
    )
    print(f"每日构建已启动（后台 worker，日志 {LOG_FILE}）")


if __name__ == "__main__":
    main()
