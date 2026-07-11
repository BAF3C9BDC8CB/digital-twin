#!/usr/bin/env python3
"""
dt-embed gRPC server — BGE-M3 text → vector conversion.
Listens on localhost:50052.

Usage:
    python3 server.py
    python3 server.py --port 50052
    python3 server.py --device cpu
"""

import argparse
import logging
import sys
import os
from concurrent import futures

import grpc
import numpy as np

# Add current dir to path for generated pb2 imports
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import embed_pb2
import embed_pb2_grpc
import common_pb2

from dt_embed.engine import EmbedEngine, get_engine

logging.basicConfig(
    level=logging.INFO,
    format="[dt-embed] %(asctime)s %(levelname)s %(message)s",
)
logger = logging.getLogger("dt-embed.server")


class EmbedServiceImpl(embed_pb2_grpc.EmbedServiceServicer):
    """gRPC service wrapping the BGE-M3 embedding engine."""

    def __init__(self, engine: EmbedEngine):
        self.engine = engine

    def Embed(self, request, context):
        """Generate embeddings for one or more texts."""
        texts = list(request.texts)
        if not texts:
            return embed_pb2.EmbedResponse(embeddings=[], model_used=self.engine.model_name)

        logger.info("Embed: %d texts", len(texts))

        try:
            vectors: np.ndarray = self.engine.encode(texts)
        except Exception as e:
            logger.error("Embed failed: %s", e)
            context.set_code(grpc.StatusCode.INTERNAL)
            context.set_details(str(e))
            return embed_pb2.EmbedResponse()

        embeddings = []
        for i in range(vectors.shape[0]):
            embedding = embed_pb2.Embedding()
            embedding.vector.extend(vectors[i].tolist())
            embeddings.append(embedding)

        logger.info("Embed: %d vectors (dim=%d)", len(embeddings), vectors.shape[1])
        return embed_pb2.EmbedResponse(
            embeddings=embeddings,
            model_used=self.engine.model_name,
        )

    def Health(self, request, context):
        """Health check."""
        if self.engine.ready:
            return common_pb2.Empty()
        context.set_code(grpc.StatusCode.UNAVAILABLE)
        context.set_details("model not loaded")
        return common_pb2.Empty()


def serve(port: int = 50052, max_workers: int = 4):
    """Start the gRPC server."""
    engine = get_engine()
    if not engine.ready:
        logger.info("Loading model %s ...", engine.model_name)
        engine.load()
    else:
        logger.info("Model already loaded (dim=%d)", engine.dim)

    server = grpc.server(
        futures.ThreadPoolExecutor(max_workers=max_workers),
        options=[
            ("grpc.max_send_message_length", 256 * 1024 * 1024),
            ("grpc.max_receive_message_length", 64 * 1024 * 1024),
        ],
    )
    embed_pb2_grpc.add_EmbedServiceServicer_to_server(EmbedServiceImpl(engine), server)

    addr = f"[::1]:{port}"
    server.add_insecure_port(addr)
    server.start()

    logger.info("dt-embed gRPC server listening on %s (model=%s, dim=%d, workers=%d)",
                 addr, engine.model_name, engine.dim, max_workers)

    try:
        server.wait_for_termination()
    except KeyboardInterrupt:
        logger.info("Shutting down...")
        server.stop(grace=5)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="dt-embed gRPC server")
    parser.add_argument("--port", type=int, default=50052, help="gRPC listen port (default: 50052)")
    parser.add_argument("--workers", type=int, default=4, help="Max thread pool workers (default: 4)")
    parser.add_argument("--device", default=None, help="Device override (cuda/cpu)")
    args = parser.parse_args()

    if args.device:
        os.environ["EMBED_DEVICE"] = args.device

    serve(port=args.port, max_workers=args.workers)
