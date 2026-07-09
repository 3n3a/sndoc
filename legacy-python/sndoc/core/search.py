"""Query orchestration: ensure the index is available locally, embed the query
with the same model used to build the index, and run the hybrid search. The
index covers the latest release only (other versions are fetched on demand via
fetch.py).
"""

from __future__ import annotations

from typing import Optional

from .constants import index_db_path
from .embed import embed_query
from .index_store import IndexStore
from .models import SearchHit

_store: Optional[tuple[str, IndexStore]] = None


def _get_store() -> IndexStore:
    global _store
    file = str(index_db_path())
    if _store is not None and _store[0] == file:
        return _store[1]
    if _store is not None:
        _store[1].close()
    _store = (file, IndexStore.open(file))
    return _store[1]


def search(query: str, limit: int = 8) -> list[SearchHit]:
    q = query.strip()
    if not q:
        return []
    store = _get_store()
    vec = embed_query(q)
    return store.query(q, vec, limit)
