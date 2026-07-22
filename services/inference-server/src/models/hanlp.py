"""HanLP NLP model loader — lightweight keyword-based NER and text analysis.

The actual NLP logic is in the TaskRouter dispatch method. This module
provides a model loader for registration in ModelRegistry so the HanLP
pipeline follows the same lifecycle as other models. If the `hanlp`
Python package is available at import time, it is used; otherwise a
lightweight keyword-matching fallback runs entirely in _dispatch_hanlp.
"""

import logging
import time
from typing import Any

logger = logging.getLogger("dt-inference.models.hanlp")

DEFAULT_HANLP_MODEL = "dt-hanlp"  # virtual model name


def load_hanlp_model() -> tuple[Any, int, str | None]:
    """Load (or prepare) the HanLP NLP pipeline.

    If the ``hanlp`` package is installed, a full multi-task pipeline is
    loaded.  Otherwise a sentinel dict is returned to indicate that the
    keyword-based fallback should be used.

    Returns:
        (model, load_ms, error)
    """
    t0 = time.time()
    model: Any = None
    error: str | None = None

    try:
        import hanlp  # type: ignore[import-untyped]

        model = hanlp.load(
            hanlp.pretrained.mtl.CLOSE_TOK_POS_NER_SRL_DEP_SDP_CON_ELECTRA_BASE_ZH
        )
        logger.info("HanLP pipeline loaded (%s)", hanlp.__version__)
    except ImportError:
        model = {"type": "keyword_matcher", "provider": "fallback"}
        logger.info("hanlp package not installed; using keyword-based fallback")
    except Exception as exc:
        error = str(exc)
        model = {"type": "keyword_matcher", "provider": "fallback"}
        logger.warning("HanLP load failed (%s); using keyword-based fallback", error)

    load_ms = int((time.time() - t0) * 1000)
    return model, load_ms, error
