"""Text embeddings via model2vec static embeddings.

No transformer forward pass, no onnxruntime: a token->vector lookup plus mean
pooling. The model (~30 MB) loads in well under a second and runs on CPU. Used
by the indexer to embed every chunk and by search to embed the query, with the
SAME model so vectors are comparable.

potion is symmetric — unlike bge, there is NO query instruction prefix. Output
vectors are L2-normalized, so cosine == dot product.
"""

from __future__ import annotations

import numpy as np

from .constants import EMBED_MODEL

# Lazily load the model so commands that never embed (e.g. a fetch-only call)
# don't pay the load cost. Downloaded from Hugging Face on first use and cached
# in the HF cache; loaded once per process and reused.
_model = None


def _get_model():
    global _model
    if _model is None:
        from model2vec import StaticModel

        _model = StaticModel.from_pretrained(EMBED_MODEL)
    return _model


def embed_passages(texts: list[str]) -> list[np.ndarray]:
    """Embed passages (documents). Returns one float32 vector per input."""
    if not texts:
        return []
    vecs = _get_model().encode(texts)
    return [np.asarray(v, dtype=np.float32) for v in vecs]


def embed_query(query: str) -> np.ndarray:
    """Embed a single search query (no prefix for potion)."""
    vec = _get_model().encode([query])[0]
    return np.asarray(vec, dtype=np.float32)


def to_vec_blob(vec: np.ndarray) -> bytes:
    """Pack an embedding into the little-endian float32 blob sqlite-vec expects."""
    return np.ascontiguousarray(vec, dtype="<f4").tobytes()
