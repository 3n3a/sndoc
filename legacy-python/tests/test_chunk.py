"""Chunking: heading splits, anchors, windowing, title-only fallback, embed text."""

from __future__ import annotations

from sndoc.core.chunk import chunk_doc, chunk_embed_text, slug

DOC = """---
title: GlideRecord
breadcrumb:
  - API
  - GlideRecord
canonical_url: https://example.com/r/api/glide-record
---

Intro paragraph about GlideRecord.

# Query Methods

Use addQuery to filter.

## get()

Fetch a single record.
"""


def test_chunks_split_by_heading_with_anchors():
    chunks = chunk_doc(DOC, "api/glide-record.md", "zurich")
    headings = [c.heading for c in chunks]
    assert "Query Methods" in headings
    assert "get()" in headings

    by_heading = {c.heading: c for c in chunks}
    assert by_heading["Query Methods"].anchor == "query-methods"
    assert by_heading["get()"].anchor == "get"

    # Metadata propagates to every chunk.
    for c in chunks:
        assert c.title == "GlideRecord"
        assert c.breadcrumb == "API > GlideRecord"
        assert c.release == "zurich"
        assert c.path == "api/glide-record.md"


def test_lead_section_has_empty_anchor():
    chunks = chunk_doc(DOC, "api/glide-record.md", "zurich")
    lead = [c for c in chunks if "Intro paragraph" in c.content]
    assert lead and lead[0].anchor == ""


def test_title_only_doc_still_yields_a_chunk():
    doc = "---\ntitle: Empty Topic\nbreadcrumb: Docs\n---\n"
    chunks = chunk_doc(doc, "empty.md", "tokyo")
    assert len(chunks) == 1
    assert chunks[0].title == "Empty Topic"


def test_long_section_is_windowed():
    body = "x " * 2000  # ~4000 chars, well over MAX_CHARS
    doc = f"---\ntitle: Big\n---\n\n# Section\n\n{body}\n"
    chunks = chunk_doc(doc, "big.md", "utah")
    assert len(chunks) > 1
    assert all(len(c.content) <= 1600 for c in chunks)


def test_embed_text_prefixes_context_without_duplicates():
    chunks = chunk_doc(DOC, "api/glide-record.md", "zurich")
    text = chunk_embed_text(chunks[0])
    # breadcrumb + title context comes first, then the body.
    assert text.startswith("API > GlideRecord — GlideRecord")


def test_slug():
    assert slug("Hello, World!") == "hello-world"
    assert slug("get()") == "get"
