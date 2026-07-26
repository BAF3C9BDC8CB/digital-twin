"""Deployment service — handles K8s deployment and health checks.

@knowledge domain="部署" concept="healthcheck" definition="服务健康检查端点，K8s 通过 /healthz 探测容器存活状态"
@knowledge domain="部署" concept="healthcheck" pitfall="健康检查端点必须在 5 秒内响应，否则 K8s 会杀掉容器"
@knowledge domain="部署" concept="gracefulShutdown" definition="优雅关闭，收到 SIGTERM 后先停止接收新请求再处理完已有请求"
@knowledge domain="部署" concept="gracefulShutdown" pitfall="K8s 滚动更新时，如果 terminationGracePeriodSeconds 设置太短会导致请求被中断"
"""

from flask import Flask, request, jsonify

app = Flask(__name__)


def check_health():
    """Check service health — K8s liveness probe endpoint."""
    return {"status": "ok"}


def graceful_shutdown():
    """Graceful shutdown — stop accepting new requests, finish pending ones."""
    # Stop accepting new connections
    # Wait for pending requests to complete
    return "shutting down"


def deploy_service(image, tag):
    """Deploy a new version of the service to K8s."""
    # Rolling update
    return f"deployed {image}:{tag}"


def rollback_deployment(version):
    """Rollback to a previous deployment version."""
    return f"rolled back to {version}"