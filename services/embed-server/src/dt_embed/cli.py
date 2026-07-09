"""
命令行入口: dt-embed
用法:
    dt-embed "单条文本"
    dt-embed "文本1" "文本2" "文本3"
    cat texts.txt | dt-embed
    echo '["text1","text2"]' | dt-embed --json
    dt-embed --file input.txt --output output.json
    dt-embed --info
    dt-embed --daemon           # 常驻后台进程，Unix socket JSON-line
"""

import argparse
import json
import logging
import os
import socket
import sys
import time

import orjson

from .engine import EmbedEngine, get_engine, DEVICE, MODEL_NAME
from .pipeline import Pipeline, DEFAULT_CHUNK_SIZE

logging.basicConfig(
    level=logging.INFO,
    format="[%(name)s] %(message)s",
    stream=sys.stderr,
)
logger = logging.getLogger("dt_embed.cli")

# Unix domain socket 路径（与 Rust 端保持一致）
SOCKET_PATH = os.environ.get("EMBED_SOCKET", "/tmp/dt-embed.sock")


def _read_stdin_lines() -> list[str]:
    """从 stdin 读取，每行一条文本（去空行）。"""
    lines = []
    for line in sys.stdin:
        stripped = line.strip()
        if stripped:
            lines.append(stripped)
    return lines


def _read_stdin_json() -> list[str]:
    """从 stdin 读取 JSON 字符串数组。"""
    raw = sys.stdin.read()
    data = json.loads(raw)
    if not isinstance(data, list):
        raise ValueError("stdin JSON 必须是字符串数组")
    return [str(item) for item in data]


def _read_file(path: str) -> list[str]:
    """读取文件，每行一条。"""
    with open(path, "r", encoding="utf-8") as f:
        return [line.strip() for line in f if line.strip()]


def cmd_info(engine: EmbedEngine):
    """打印模型信息。"""
    sys.stdout.buffer.write(orjson.dumps({
        "model": engine.model_name,
        "dim": engine.dim,
        "device": DEVICE,
        "fp16": engine.fp16,
        "compiled": engine.compiled,
        "ready": engine.ready,
    }, ensure_ascii=False, indent=2))


def cmd_encode(engine: EmbedEngine, texts: list[str]):
    """编码文本列表，返回 numpy 数组。"""
    if not texts:
        return None

    t0 = time.time()
    pipe = Pipeline(texts, on_progress=lambda done, total: _print_progress(done, total))
    pipe.run(engine)
    elapsed = time.time() - t0

    result = pipe.result_as_numpy()
    logger.info("完成 %d 条 %.1fs (%.0f 条/s)",
                 len(texts), elapsed, len(texts) / max(elapsed, 0.001))
    return result


def _print_progress(done: int, total: int):
    """简单进度输出到 stderr。"""
    pct = done / max(total, 1) * 100
    logger.info("进度 %d/%d (%.1f%%)", done, total, pct)


def _process_request(engine: EmbedEngine, req: dict, chunk_size: int) -> dict:
    """处理单个请求，返回响应 dict。"""
    # 健康检查
    if req.get("health"):
        return {
            "model": engine.model_name,
            "dim": engine.dim,
            "ready": engine.ready,
        }

    texts = req.get("texts", [])
    if not texts:
        return {"vectors": []}

    if len(texts) <= chunk_size:
        vecs = engine.encode(texts)
    else:
        pipe = Pipeline(texts, chunk_size=chunk_size)
        pipe.run(engine)
        vecs = pipe.result_as_numpy()

    return {"vectors": vecs}


def _recv_exact(conn: socket.socket, size: int) -> bytes:
    """精确接收 size 字节。"""
    buf = b""
    while len(buf) < size:
        chunk = conn.recv(size - len(buf))
        if not chunk:
            raise ConnectionError("客户端断开")
        buf += chunk
    return buf


def _recv_frame(conn: socket.socket) -> bytes:
    """读取一个长度前缀帧：4 字节大端长度 + payload。"""
    header = _recv_exact(conn, 4)
    payload_len = int.from_bytes(header, "big")
    return _recv_exact(conn, payload_len)


def _send_frame(conn: socket.socket, data: bytes):
    """发送一个长度前缀帧：4 字节大端长度 + payload。"""
    conn.sendall(len(data).to_bytes(4, "big") + data)


def cmd_daemon(engine: EmbedEngine, chunk_size: int):
    """
    常驻后台进程模式。
    监听 Unix domain socket，接受连接，每连接处理一个请求。
    帧协议: 4 字节大端长度 + JSON payload。
    """
    # 清理旧 socket 文件
    if os.path.exists(SOCKET_PATH):
        os.unlink(SOCKET_PATH)

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(SOCKET_PATH)
    server.listen(5)
    # 设置文件权限让同用户可读写
    os.chmod(SOCKET_PATH, 0o600)

    logger.info("daemon 已启动，监听 %s", SOCKET_PATH)

    try:
        while True:
            conn, _ = server.accept()
            try:
                # 读取请求帧
                raw = _recv_frame(conn)
                req = orjson.loads(raw)

                # 处理
                resp = _process_request(engine, req, chunk_size)

                # 发送响应帧
                data = orjson.dumps(resp, option=orjson.OPT_SERIALIZE_NUMPY)
                _send_frame(conn, data)
            except Exception as e:
                logger.error("请求处理失败: %s", e)
                try:
                    err = orjson.dumps({"error": str(e)})
                    _send_frame(conn, err)
                except Exception:
                    pass
            finally:
                conn.close()
    except KeyboardInterrupt:
        pass
    finally:
        server.close()
        if os.path.exists(SOCKET_PATH):
            os.unlink(SOCKET_PATH)
        logger.info("daemon 退出")


def main():
    parser = argparse.ArgumentParser(
        prog="dt-embed",
        description="本地 GPU 文本向量化工具 (BGE-M3)",
    )
    parser.add_argument("texts", nargs="*", help="待编码文本（命令行参数）")
    parser.add_argument("--json", action="store_true", help="从 stdin 读取 JSON 数组")
    parser.add_argument("--file", "-f", help="从文件读取（每行一条）")
    parser.add_argument("--output", "-o", help="输出 JSON 文件路径（默认 stdout）")
    parser.add_argument("--info", action="store_true", help="打印模型信息并退出")
    parser.add_argument("--daemon", action="store_true", help="常驻后台进程模式（Unix socket）")
    parser.add_argument("--no-progress", action="store_true", help="不输出进度")
    parser.add_argument("--chunk-size", type=int, default=DEFAULT_CHUNK_SIZE,
                        help=f"分块大小 (默认 {DEFAULT_CHUNK_SIZE})")
    args = parser.parse_args()

    # --daemon
    if args.daemon:
        engine = get_engine()
        if not engine.ready:
            engine.load()
        cmd_daemon(engine, args.chunk_size)
        return

    # --info
    if args.info:
        engine = get_engine()
        if not engine.ready:
            engine.load()
        cmd_info(engine)
        return

    # 收集输入（优先级: --file > 位置参数 > stdin）
    texts: list[str] = []

    if args.file:
        texts = _read_file(args.file)
    elif args.texts:
        texts = list(args.texts)
    elif not sys.stdin.isatty():
        if args.json:
            texts = _read_stdin_json()
        else:
            texts = _read_stdin_lines()
    else:
        parser.print_help()
        sys.exit(1)

    if not texts:
        logger.warning("无输入文本")
        sys.exit(0)

    # 编码
    engine = get_engine()
    if not engine.ready:
        engine.load()

    result = cmd_encode(engine, texts)

    # 输出 (orjson 序列化 numpy 数组，零拷贝)
    data = orjson.dumps(result, option=orjson.OPT_SERIALIZE_NUMPY)
    if args.output:
        with open(args.output, "wb") as f:
            f.write(data)
        logger.info("已写入 %s (%d 条向量)", args.output, len(result))
    else:
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.write(b"\n")


if __name__ == "__main__":
    main()
