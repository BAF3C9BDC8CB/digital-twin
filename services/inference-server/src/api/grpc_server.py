"""gRPC server implementations — legacy protocol compatibility.

Uses a SharedEventLoop to bridge gRPC threads with the asyncio queue,
avoiding the asyncio.run() per-request overhead.
"""

import asyncio
import logging
import threading
from concurrent import futures

import grpc

from taskqueue.task import Priority

logger = logging.getLogger("dt-inference.grpc")


class SharedEventLoop:
    """Bridge between gRPC thread pool and asyncio event loop.

    gRPC handlers run in a ThreadPoolExecutor. This class lets them
    safely schedule coroutines on the asyncio event loop without
    creating a new loop per request.
    """

    def __init__(self):
        self.loop: asyncio.AbstractEventLoop = None
        self._ready = threading.Event()

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self.loop = loop
        self._ready.set()

    def run_coro(self, coro, timeout: float = 30):
        """Schedule a coroutine from a gRPC thread safely."""
        self._ready.wait()
        future = asyncio.run_coroutine_threadsafe(coro, self.loop)
        return future.result(timeout=timeout)


def create_grpc_servicer_classes(router, shared_loop: SharedEventLoop):
    """Create gRPC service classes with the given router and event loop bridge."""

    import embed_pb2
    import embed_pb2_grpc
    import reranker_pb2
    import reranker_pb2_grpc
    import common_pb2

    class LegacyEmbedServiceImpl(embed_pb2_grpc.EmbedServiceServicer):
        def Embed(self, request, context):
            texts = list(request.texts)
            if not texts:
                return embed_pb2.EmbedResponse(embeddings=[])
            try:
                from models.embed import DEFAULT_EMBED_MODEL
                coro = router.submit(
                    "embed", {"texts": texts}, priority=Priority.NORMAL
                )
                result = shared_loop.run_coro(coro)
                embeddings = []
                for vec in result["embeddings"]:
                    e = embed_pb2.Embedding()
                    e.vector.extend(vec)
                    embeddings.append(e)
                return embed_pb2.EmbedResponse(
                    embeddings=embeddings, model_used=DEFAULT_EMBED_MODEL
                )
            except Exception as e:
                logger.error("Legacy embed failed: %s", e)
                context.set_code(grpc.StatusCode.INTERNAL)
                context.set_details(str(e))
                return embed_pb2.EmbedResponse()

        def Health(self, request, context):
            return common_pb2.Empty()

    class LegacyRerankerServiceImpl(reranker_pb2_grpc.RerankerServiceServicer):
        def Rerank(self, request, context):
            query = request.query
            texts = list(request.texts)
            if not query or not texts:
                return reranker_pb2.RerankResponse(scores=[])
            try:
                from models.reranker import DEFAULT_RERANKER_MODEL
                coro = router.submit(
                    "rerank", {"query": query, "texts": texts},
                    priority=Priority.NORMAL,
                )
                result = shared_loop.run_coro(coro)
                return reranker_pb2.RerankResponse(
                    scores=result["scores"], model_used=DEFAULT_RERANKER_MODEL,
                )
            except Exception as e:
                logger.error("Legacy rerank failed: %s", e)
                context.set_code(grpc.StatusCode.INTERNAL)
                context.set_details(str(e))
                return reranker_pb2.RerankResponse()

        def Health(self, request, context):
            return common_pb2.Empty()

    return LegacyEmbedServiceImpl, LegacyRerankerServiceImpl


def serve_grpc(
    router,
    shared_loop: SharedEventLoop,
    port: int = 50051,
    max_workers: int = 4,
):
    """Start gRPC server on the given port."""
    import embed_pb2_grpc
    import reranker_pb2_grpc
    import inference_pb2_grpc

    from models.embed import DEFAULT_EMBED_MODEL
    from models.reranker import DEFAULT_RERANKER_MODEL
    from models.llm import DEFAULT_LLM_MODEL
    from taskqueue.router import QUEUE_MAXSIZE

    EmbedServicer, RerankerServicer = create_grpc_servicer_classes(router, shared_loop)

    # Placeholder for unified inference service (future)
    class InferenceServicer(inference_pb2_grpc.InferenceServiceServicer):
        pass

    grpc_server = grpc.server(
        futures.ThreadPoolExecutor(max_workers=max_workers),
        options=[
            ("grpc.max_send_message_length", 256 * 1024 * 1024),
            ("grpc.max_receive_message_length", 64 * 1024 * 1024),
        ],
    )
    embed_pb2_grpc.add_EmbedServiceServicer_to_server(
        EmbedServicer(), grpc_server,
    )
    reranker_pb2_grpc.add_RerankerServiceServicer_to_server(
        RerankerServicer(), grpc_server,
    )
    inference_pb2_grpc.add_InferenceServiceServicer_to_server(
        InferenceServicer(), grpc_server,
    )

    grpc_addr = f"[::1]:{port}"
    grpc_server.add_insecure_port(grpc_addr)
    grpc_server.start()

    logger.info("dt-inference gRPC  listening on %s (workers=%d)", grpc_addr, max_workers)
    logger.info("  Embed:   %s", DEFAULT_EMBED_MODEL)
    logger.info("  Reranker: %s", DEFAULT_RERANKER_MODEL)
    logger.info("  LLM:     %s", DEFAULT_LLM_MODEL)
    logger.info("  Queue:   maxsize=%d", QUEUE_MAXSIZE)

    return grpc_server
