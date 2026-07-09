"""IndexStore build + hybrid query: RRF fusion, best-chunk-per-file dedup,
ordering, and limit. Uses deterministic unit vectors instead of the model so the
test is offline and fast."""

from __future__ import annotations

import numpy as np
import pytest

from sndoc.core.constants import EMBED_DIM
from sndoc.core.index_store import IndexStore
from sndoc.core.models import IndexChunk


def _unit(dim: int) -> np.ndarray:
    v = np.zeros(EMBED_DIM, dtype=np.float32)
    v[dim] = 1.0
    return v


def _chunk(path: str, heading: str, content: str) -> IndexChunk:
    return IndexChunk(
        path=path,
        anchor=heading.lower(),
        title=path,
        breadcrumb="Docs",
        heading=heading,
        release="zurich",
        content=content,
        canonical_url="",
        last_updated="",
    )


@pytest.fixture
def store(tmp_path):
    chunks = [
        _chunk("a.md", "Alpha", "alpha beta gamma"),
        _chunk("a.md", "Alpha Two", "alpha extended discussion"),
        _chunk("b.md", "Delta", "delta epsilon zeta"),
    ]
    vecs = [_unit(0), _unit(1), _unit(2)]
    s = IndexStore.create(str(tmp_path / "idx.db"))
    s.insert_chunks(chunks, vecs)
    s.finalize()
    yield s
    s.close()


def test_query_returns_best_match_first(store):
    hits = store.query("alpha", _unit(0), n=8)
    assert hits
    assert hits[0].path == "a.md"


def test_query_dedupes_to_one_chunk_per_file(store):
    hits = store.query("alpha", _unit(0), n=8)
    paths = [h.path for h in hits]
    assert len(paths) == len(set(paths))  # no file appears twice


def test_query_respects_limit(store):
    hits = store.query("alpha delta", _unit(0), n=1)
    assert len(hits) == 1


def test_vector_arm_finds_unrelated_file(store):
    # A query whose vector points at b.md's embedding surfaces b.md even with no
    # lexical overlap.
    hits = store.query("zzzznomatch", _unit(2), n=8)
    assert any(h.path == "b.md" for h in hits)
