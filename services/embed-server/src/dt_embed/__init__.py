"""
dt_embed — 本地 GPU 文本向量化工具。
"""

from .engine import EmbedEngine, get_engine
from .pipeline import Pipeline

__version__ = "3.0.0"
__all__ = ["EmbedEngine", "get_engine", "Pipeline"]
