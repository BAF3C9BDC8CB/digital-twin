#!/usr/bin/env python3
"""Hermes post_tool_call hook — 代码文件修改后自动触发 dt 增量构建。

触发: Hermes 执行 write_file/patch 工具后（config.yaml hooks 注册）。
行为: 本脚本立即返回（不阻塞 agent），把被修改文件所属项目入队，
      然后 spawn 一个脱离会话的后台聚合进程：等待 3 秒收集同一批
      变更，再对该项目执行一次 `dt build --path --name`（增量：
      仅处理变更文件，mtime 快速路径）。

为什么不是 watcher: 不监听文件系统事件（无 inotify 风暴），只在
Hermes 实际改文件时触发。手动修改场景由 git post-commit hook 或
手动 `dt build` 兜底。

协议: stdin 收 JSON payload，stdout 输出 `{}`（不阻塞 agent 循环）。
"""

import json
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
QUEUE_DIR = Path(os.environ.get("DT_BUILD_QUEUE_DIR", "/tmp/dt-build-queue"))
AGGREGATE_WAIT = float(os.environ.get("DT_BUILD_AGGREGATE_WAIT", "3"))  # 聚合窗口
LOCK_FILE = QUEUE_DIR / ".aggregator.lock"
LOG_FILE = QUEUE_DIR / "auto-build.log"

# 只处理源码/文档扩展名（.md 配置说明等不触发构建）
SOURCE_EXTS = {
    "java", "py", "ts", "tsx", "go", "rs", "php", "js", "jsx", "mjs", "cjs",
    "kt", "kts", "swift", "scala", "rb", "cpp", "cc", "cxx", "c", "h", "hpp",
    "cs", "fs", "fsx", "vue", "svelte", "md", "txt", "yaml", "yml", "properties",
    "sql", "sh", "xml", "html", "css", "scss",
}


def log(msg: str) -> None:
    try:
        QUEUE_DIR.mkdir(parents=True, exist_ok=True)
        with open(LOG_FILE, "a") as f:
            f.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {msg}\n")
    except Exception:
        pass


def load_projects() -> list[tuple[str, Path]]:
    """从 config.yaml 解析 (项目名, 项目根) 列表（与 dt resolve_project_paths 同构）。"""
    try:
        import yaml
    except ImportError:
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


def resolve_project(file_path: Path, projects: list[tuple[str, Path]]):
    """最长前缀匹配：返回文件所属项目（如 copartner/copartner-h5 → copartner）。"""
    best = None
    for name, root in projects:
        try:
            file_path.relative_to(root)
            if best is None or len(root.parts) > len(best[1].parts):
                best = (name, root)
        except ValueError:
            pass
    return best


def enqueue(project: str) -> None:
    QUEUE_DIR.mkdir(parents=True, exist_ok=True)
    marker = QUEUE_DIR / f"{project}.queue"
    with open(marker, "w") as f:
        f.write(str(time.time()))


def _spawn_aggregator() -> None:
    """以脱离会话的后台进程运行聚合构建（setsid：不随 Hermes 退出）。"""
    script = Path(__file__).resolve()
    env = dict(os.environ)
    env["DT_AGGREGATOR_ROLE"] = "aggregate"
    try:
        subprocess.Popen(
            [sys.executable, str(script)],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
            close_fds=True,
        )
    except Exception as e:
        log(f"spawn 聚合进程失败: {e}")


def aggregate_and_build() -> None:
    """后台聚合进程：等聚合窗口收齐变更，然后逐个项目增量构建。"""
    time.sleep(AGGREGATE_WAIT)
    markers = list(QUEUE_DIR.glob("*.queue")) if QUEUE_DIR.exists() else []
    if not markers:
        return
    projects = load_projects()
    names = {m.stem for m in markers}
    for m in markers:
        try:
            m.unlink()
        except OSError:
            pass
    for name in names:
        root = next((p for n, p in projects if n == name), None)
        if root is None:
            log(f"跳过未知项目 {name}")
            continue
        log(f"触发增量构建: {name} @ {root}")
        try:
            r = subprocess.run(
                [DT_BIN, "build", "--path", str(root), "--name", name],
                capture_output=True,
                text=True,
                timeout=600,
            )
            log(f"构建完成 {name}: exit={r.returncode}")
        except Exception as e:
            log(f"构建失败 {name}: {e}")


def main() -> None:
    # 聚合角色：直接执行构建（由 hook 角色 spawn 的后台进程进入此分支）
    if os.environ.get("DT_AGGREGATOR_ROLE") == "aggregate":
        try:
            aggregate_and_build()
        finally:
            try:
                LOCK_FILE.unlink()
            except OSError:
                pass
        return

    # ── hook 角色：解析 payload，入队，spawn 聚合进程，立即返回 ──
    try:
        payload = json.load(sys.stdin)
    except Exception:
        print("{}")
        return

    tool_name = payload.get("tool_name", "")
    if tool_name not in ("write_file", "patch"):
        print("{}")
        return

    tool_input = payload.get("tool_input") or {}
    path_str = tool_input.get("path") or ""
    if not path_str or path_str.startswith("~"):
        print("{}")
        return
    file_path = Path(os.path.expanduser(path_str))
    if not file_path.is_absolute():
        cwd = payload.get("cwd") or os.getcwd()
        file_path = Path(cwd) / file_path
    if file_path.suffix.lstrip(".").lower() not in SOURCE_EXTS:
        print("{}")
        return

    projects = load_projects()
    hit = resolve_project(file_path, projects)
    if hit is None:
        print("{}")
        return

    enqueue(hit[0])
    log(f"入队 {hit[0]} <- {file_path}")

    # flock 语义：O_EXCL 原子创建锁；成功者 spawn 聚合进程，失败者让出
    try:
        QUEUE_DIR.mkdir(parents=True, exist_ok=True)
        fd = os.open(LOCK_FILE, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
        os.write(fd, str(os.getpid()).encode())
        os.close(fd)
    except FileExistsError:
        print("{}")
        return

    _spawn_aggregator()
    print("{}")


if __name__ == "__main__":
    main()
