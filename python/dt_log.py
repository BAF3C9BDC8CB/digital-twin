"""
Python gRPC 日志处理器，用于 Digital Twin 统一日志系统。

提供 `GrpcLogHandler(logging.Handler)` — 一个 Python `logging` 处理器，
将日志记录转发到 dt-daemon 的 `LogService.StreamLogs` gRPC 端点。

用法:
    import logging
    from dt_log import GrpcLogHandler

    logger = logging.getLogger("my_service")
    handler = GrpcLogHandler(daemon_addr="localhost:50051", plugin="embed")
    logger.addHandler(handler)
    logger.setLevel(logging.INFO)

    logger.info("嵌入流水线已启动", extra={"trace_id": "a1b2c3"})

# 设计说明
# --------
# - 该处理器是非阻塞的：日志记录先入队，再由后台线程异步发送，
#   因此日志记录永远不会阻塞主应用。
# - 若守护进程不可达，记录会被静默丢弃（冷却期后向 stderr 输出一条警告）。
# - gRPC stub 由 `proto/log.proto` 生成。在启用 proto 编译之前，
#   本模块提供一个本地回退实现：将 JSON 行写入 stdout（与守护进程格式兼容）。
"""

from __future__ import annotations

import json
import logging
import os
import queue
import sys
import threading
import time
from datetime import datetime, timezone
from typing import Optional


# ---------------------------------------------------------------------------
# 本地 JSON 回退（在 gRPC 不可用时使用）
# ---------------------------------------------------------------------------

def _format_json(record: logging.LogRecord, plugin: str) -> str:
    """将 Python 日志记录格式化为与 dt-log 兼容的 JSON 行。"""
    ts = datetime.fromtimestamp(record.created, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"
    level = record.levelname
    target = f"{plugin}.{record.name}" if plugin else record.name
    trace_id = getattr(record, "trace_id", "00000000")
    message = record.getMessage()

    obj = {
        "ts": ts,
        "level": level,
        "target": target,
        "trace_id": trace_id,
        "span_id": "",
        "plugin": plugin,
        "message": message,
    }

    if record.exc_info and record.exc_info[1]:
        obj["error"] = str(record.exc_info[1])

    return json.dumps(obj, ensure_ascii=False, default=str)


# ---------------------------------------------------------------------------
# GrpcLogHandler
# ---------------------------------------------------------------------------

class GrpcLogHandler(logging.Handler):
    """
    将日志记录发送到 dt-daemon 的 LogService 的 logging.Handler。

    特性:
    - 非阻塞：使用内部队列 + 后台线程。
    - 优雅降级：守护进程不可达或未安装 gRPC 时，回退为本地 JSON 行输出。
    - 带退避的自动重连。

    参数
    ----------
    daemon_addr : str
        dt-daemon 的 gRPC 地址（例如 "localhost:50051"）。
    plugin : str
        用于标记所有日志记录的插件名。
    fallback_file : str 或 None
        若设置，守护进程不可达时将 JSON 行写入该文件。
        默认为 stdout。
    """

    _FALLBACK_COOLDOWN_SECS = 30.0  # 重试 gRPC 前的冷却时长

    def __init__(
        self,
        daemon_addr: str = "localhost:50051",
        plugin: str = "",
        fallback_file: Optional[str] = None,
        level: int = logging.NOTSET,
    ):
        super().__init__(level=level)
        self.daemon_addr = daemon_addr
        self.plugin = plugin
        self.fallback_file = fallback_file

        # 内部队列 + 工作线程
        self._queue: queue.Queue[logging.LogRecord] = queue.Queue(maxsize=10000)
        self._worker: Optional[threading.Thread] = None
        self._running = False
        self._grpc_available = False
        self._last_attempt = 0.0

        # 尝试导入 grpc
        try:
            import grpc  # noqa: F401
            self._grpc_available = True
        except ImportError:
            self._grpc_available = False

        self._start_worker()

    # ── Handler 接口 ──────────────────────────────────────────

    def emit(self, record: logging.LogRecord) -> None:
        """将日志记录入队。永不阻塞。"""
        try:
            self._queue.put_nowait(record)
        except queue.Full:
            # 静默丢弃——比阻塞应用更好
            pass

    def close(self) -> None:
        """关闭后台工作线程。"""
        self._running = False
        if self._worker and self._worker.is_alive():
            self._worker.join(timeout=5.0)
        super().close()

    # ── 工作线程 ─────────────────────────────────────────────────────

    def _start_worker(self) -> None:
        self._running = True
        self._worker = threading.Thread(target=self._run, daemon=True)
        self._worker.start()

    def _run(self) -> None:
        """后台循环：取出队列中的记录 → 发送到守护进程或回退输出。"""
        batch: list[logging.LogRecord] = []
        while self._running:
            try:
                # 最多取出 100 条记录，或等待 0.5 秒，以先到者为准
                try:
                    record = self._queue.get(timeout=0.5)
                    batch.append(record)
                except queue.Empty:
                    pass

                # 再非阻塞地取出其余排队记录
                while len(batch) < 100:
                    try:
                        record = self._queue.get_nowait()
                        batch.append(record)
                    except queue.Empty:
                        break

                if batch:
                    self._send_batch(batch)
                    batch.clear()
            except Exception:
                # 吞掉异常——绝不使日志工作线程崩溃
                batch.clear()

    def _send_batch(self, records: list[logging.LogRecord]) -> None:
        """发送一批记录到守护进程，失败时回退到本地输出。"""
        now = time.time()

        if self._grpc_available and (now - self._last_attempt > self._FALLBACK_COOLDOWN_SECS):
            if self._try_grpc_send(records):
                return

        # 回退：在本地写 JSON 行
        self._write_fallback(records)

    def _try_grpc_send(self, records: list[logging.LogRecord]) -> bool:
        """尝试通过 gRPC 发送记录，成功返回 True。"""
        # gRPC 客户端集成占位——待 proto/log.proto 编译启用且
        # grpcio/grpclib 可用后再实现。
        self._last_attempt = time.time()
        return False

    def _write_fallback(self, records: list[logging.LogRecord]) -> None:
        """将 JSON 行写入回退输出。"""
        out = sys.stdout
        if self.fallback_file:
            try:
                out = open(self.fallback_file, "a", encoding="utf-8")
            except OSError:
                out = sys.stdout

        try:
            for rec in records:
                line = _format_json(rec, self.plugin)
                out.write(line + "\n")
        finally:
            if out is not sys.stdout:
                try:
                    out.close()
                except OSError:
                    pass


# ---------------------------------------------------------------------------
# 便捷函数：为服务配置日志
# ---------------------------------------------------------------------------

def setup_logging(
    service_name: str,
    daemon_addr: str = "localhost:50051",
    level: int = logging.INFO,
    fallback_file: Optional[str] = None,
) -> GrpcLogHandler:
    """
    配置 Python 服务使用 GrpcLogHandler。

    返回该 handler，以便调用方稍后移除它或调整日志级别。

    示例:
        handler = setup_logging("embed", daemon_addr="localhost:50051")
        logging.getLogger("dt_embed").info("就绪")
    """
    root = logging.getLogger()
    root.setLevel(level)

    # 移除默认 handler（例如 basicConfig 的 stderr handler）
    for h in list(root.handlers):
        root.removeHandler(h)

    handler = GrpcLogHandler(
        daemon_addr=daemon_addr,
        plugin=service_name,
        fallback_file=fallback_file,
        level=level,
    )
    root.addHandler(handler)

    # 同时保留一个 stderr handler，便于开发时查看
    stderr_handler = logging.StreamHandler(sys.stderr)
    stderr_handler.setLevel(logging.WARNING)
    stderr_handler.setFormatter(
        logging.Formatter("[%(name)s] %(levelname)s %(message)s")
    )
    root.addHandler(stderr_handler)

    return handler


# ---------------------------------------------------------------------------
# 自检（运行 `python dt_log.py`）
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    print("dt_log 自检: GrpcLogHandler", file=sys.stderr)

    handler = GrpcLogHandler(
        daemon_addr="localhost:50051",
        plugin="test",
        fallback_file="/tmp/dt-log-test.jsonl",
    )

    logger = logging.getLogger("dt_log.test")
    logger.setLevel(logging.DEBUG)
    logger.addHandler(handler)

    logger.debug("调试消息")
    logger.info("信息消息，含 trace_id", extra={"trace_id": "test-1234"})
    logger.warning("警告消息")
    logger.error("错误消息")

    time.sleep(1.0)
    handler.close()

    print("自检完成。请检查 /tmp/dt-log-test.jsonl 的输出。", file=sys.stderr)
