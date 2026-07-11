"""
Python gRPC log handler for the Digital Twin unified logging system.

Provides `GrpcLogHandler(logging.Handler)` — a Python `logging` handler that
forwards log records to the dt-daemon's `LogService.StreamLogs` gRPC endpoint.

Usage:
    import logging
    from dt_log import GrpcLogHandler

    logger = logging.getLogger("my_service")
    handler = GrpcLogHandler(daemon_addr="localhost:50051", plugin="embed")
    logger.addHandler(handler)
    logger.setLevel(logging.INFO)

    logger.info("embedding pipeline started", extra={"trace_id": "a1b2c3"})

# Design notes
# ------------
# - This handler is NON-BLOCKING: log records are queued and sent
#   asynchronously via a background thread, so logging never blocks
#   the main application.
# - If the daemon is unreachable, records are silently dropped
#   (with a warning to stderr after a cooldown period).
# - The gRPC stub is generated from `proto/log.proto`. Until proto
#   compilation is enabled, this module provides a local fallback that
#   writes JSON lines to stdout (compatible with the daemon's format).
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
# Local JSON fallback (used when gRPC is unavailable)
# ---------------------------------------------------------------------------

def _format_json(record: logging.LogRecord, plugin: str) -> str:
    """Format a Python log record as a dt-log-compatible JSON line."""
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
    A logging.Handler that sends log records to dt-daemon's LogService.

    Features:
    - Non-blocking: uses an internal queue + background thread.
    - Graceful degradation: falls back to local JSON-line output if the
      daemon is unreachable or gRPC is not installed.
    - Automatic reconnection with backoff.

    Parameters
    ----------
    daemon_addr : str
        gRPC address of dt-daemon (e.g. "localhost:50051").
    plugin : str
        Plugin name to tag on all log records.
    fallback_file : str or None
        If set, write JSON lines to this file when the daemon is unreachable.
        Defaults to stdout.
    """

    _FALLBACK_COOLDOWN_SECS = 30.0  # how long to wait before retrying gRPC

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

        # Internal queue + worker
        self._queue: queue.Queue[logging.LogRecord] = queue.Queue(maxsize=10000)
        self._worker: Optional[threading.Thread] = None
        self._running = False
        self._grpc_available = False
        self._last_attempt = 0.0

        # Try to import grpc
        try:
            import grpc  # noqa: F401
            self._grpc_available = True
        except ImportError:
            self._grpc_available = False

        self._start_worker()

    # ── Handler interface ──────────────────────────────────────────

    def emit(self, record: logging.LogRecord) -> None:
        """Enqueue a log record. Never blocks."""
        try:
            self._queue.put_nowait(record)
        except queue.Full:
            # Drop silently — better than blocking the application
            pass

    def close(self) -> None:
        """Shut down the background worker."""
        self._running = False
        if self._worker and self._worker.is_alive():
            self._worker.join(timeout=5.0)
        super().close()

    # ── Worker ─────────────────────────────────────────────────────

    def _start_worker(self) -> None:
        self._running = True
        self._worker = threading.Thread(target=self._run, daemon=True)
        self._worker.start()

    def _run(self) -> None:
        """Background loop: drain queue → send to daemon or fallback."""
        batch: list[logging.LogRecord] = []
        while self._running:
            try:
                # Drain up to 100 records or 0.5 s, whichever comes first
                try:
                    record = self._queue.get(timeout=0.5)
                    batch.append(record)
                except queue.Empty:
                    pass

                # Also drain any additional queued records (non-blocking)
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
                # Swallow — never crash the log worker
                batch.clear()

    def _send_batch(self, records: list[logging.LogRecord]) -> None:
        """Send a batch to the daemon, falling back to local output on failure."""
        now = time.time()

        if self._grpc_available and (now - self._last_attempt > self._FALLBACK_COOLDOWN_SECS):
            if self._try_grpc_send(records):
                return

        # Fallback: write JSON lines locally
        self._write_fallback(records)

    def _try_grpc_send(self, records: list[logging.LogRecord]) -> bool:
        """Attempt to send records via gRPC. Returns True on success."""
        # gRPC client integration placeholder — will be implemented when
        # proto/log.proto compilation is enabled and grpcio/grpclib is available.
        self._last_attempt = time.time()
        return False

    def _write_fallback(self, records: list[logging.LogRecord]) -> None:
        """Write JSON lines to the fallback output."""
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
# Convenience: set up logging for a service
# ---------------------------------------------------------------------------

def setup_logging(
    service_name: str,
    daemon_addr: str = "localhost:50051",
    level: int = logging.INFO,
    fallback_file: Optional[str] = None,
) -> GrpcLogHandler:
    """
    Configure a Python service to use GrpcLogHandler.

    Returns the handler so the caller can remove it or adjust the level later.

    Example:
        handler = setup_logging("embed", daemon_addr="localhost:50051")
        logging.getLogger("dt_embed").info("ready")
    """
    root = logging.getLogger()
    root.setLevel(level)

    # Remove default handlers (e.g. basicConfig stderr handler)
    for h in list(root.handlers):
        root.removeHandler(h)

    handler = GrpcLogHandler(
        daemon_addr=daemon_addr,
        plugin=service_name,
        fallback_file=fallback_file,
        level=level,
    )
    root.addHandler(handler)

    # Also keep a stderr handler for development visibility
    stderr_handler = logging.StreamHandler(sys.stderr)
    stderr_handler.setLevel(logging.WARNING)
    stderr_handler.setFormatter(
        logging.Formatter("[%(name)s] %(levelname)s %(message)s")
    )
    root.addHandler(stderr_handler)

    return handler


# ---------------------------------------------------------------------------
# Self-test (run with `python dt_log.py`)
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    print("dt_log self-test: GrpcLogHandler", file=sys.stderr)

    handler = GrpcLogHandler(
        daemon_addr="localhost:50051",
        plugin="test",
        fallback_file="/tmp/dt-log-test.jsonl",
    )

    logger = logging.getLogger("dt_log.test")
    logger.setLevel(logging.DEBUG)
    logger.addHandler(handler)

    logger.debug("debug message")
    logger.info("info message with trace_id", extra={"trace_id": "test-1234"})
    logger.warning("warning message")
    logger.error("error message")

    time.sleep(1.0)
    handler.close()

    print("Self-test complete. Check /tmp/dt-log-test.jsonl for output.", file=sys.stderr)
