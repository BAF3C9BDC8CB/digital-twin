"""Prometheus metrics and structured logging."""

import logging

# Structured logging setup
logging.basicConfig(
    level=logging.INFO,
    format="[dt-inference] %(asctime)s %(levelname)s %(name)s %(message)s",
)


def generate_metrics() -> str:
    """Generate Prometheus metrics string."""
    try:
        from prometheus_client import Counter, Histogram, Gauge, generate_latest, REGISTRY

        # Ensure metrics are registered (idempotent)
        for name in [
            "dt_inference_total", "dt_inference_latency_seconds",
            "dt_inference_queue_depth",
        ]:
            try:
                REGISTRY._names_to_collectors[name]
            except KeyError:
                pass  # Will be auto-created when first used

        return generate_latest(REGISTRY).decode("utf-8")
    except Exception as e:
        return f"# error generating metrics: {e}\n"


# ── Metric instruments (created on first use) ─────────────────────────────
_metrics_initialized = False


def _ensure_metrics():
    global _metrics_initialized
    if _metrics_initialized:
        return
    try:
        from prometheus_client import Counter, Histogram, Gauge  # noqa: F811
        from prometheus_client import REGISTRY as _R

        # Check if already registered
        existing = set(_R._names_to_collectors.keys())

        if "dt_inference_total" not in existing:
            Counter(
                "dt_inference_total", "Total inference requests",
                ["model_type", "priority", "status"],
                registry=_R,
            )
        if "dt_inference_latency_seconds" not in existing:
            Histogram(
                "dt_inference_latency_seconds", "Inference latency",
                ["model_type", "priority"],
                registry=_R,
            )
        if "dt_inference_queue_depth" not in existing:
            Gauge(
                "dt_inference_queue_depth", "Current queue depth",
                registry=_R,
            )
        if "dt_inference_gpu_memory_bytes" not in existing:
            Gauge(
                "dt_inference_gpu_memory_bytes", "GPU VRAM used",
                registry=_R,
            )
        if "dt_inference_model_load_seconds" not in existing:
            Gauge(
                "dt_inference_model_load_seconds", "Model load time",
                ["model_name"],
                registry=_R,
            )

        _metrics_initialized = True
    except ImportError:
        pass
