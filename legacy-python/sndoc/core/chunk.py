"""Parse a doc file's frontmatter and split its body into heading-delimited
chunks small enough to embed. Each chunk carries the file title + breadcrumb +
its heading so the embedded text has enough standalone context to match
conceptual queries.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

import frontmatter

from .models import IndexChunk

# Target chunk size in characters, kept compact so the context prefix
# (title/breadcrumb/heading) plus body stays within the model's window.
MAX_CHARS = 1600
OVERLAP_CHARS = 160

_HEADING_RE = re.compile(r"^(#{1,3})\s+(.*)$")
_SLUG_STRIP_RE = re.compile(r"[^\w\s-]")
_SLUG_SPACE_RE = re.compile(r"\s+")
_PARA_SPLIT_RE = re.compile(r"\n{2,}")


@dataclass(slots=True)
class DocMeta:
    title: str
    breadcrumb: str
    canonical_url: str
    last_updated: str


@dataclass(slots=True)
class ParsedDoc:
    meta: DocMeta
    body: str


def _as_string(v: object) -> str:
    if v is None:
        return ""
    if isinstance(v, (list, tuple)):
        return " > ".join(str(x) for x in v)
    return str(v)


def parse_doc(markdown: str) -> ParsedDoc:
    """Split frontmatter from body and normalize the fields we index on."""
    data: dict = {}
    body = markdown
    try:
        post = frontmatter.loads(markdown)
        data = post.metadata
        body = post.content
    except Exception:
        # Malformed frontmatter: index the raw body, empty meta.
        pass
    return ParsedDoc(
        meta=DocMeta(
            title=_as_string(data.get("title")),
            breadcrumb=_as_string(data.get("breadcrumb")),
            canonical_url=_as_string(data.get("canonical_url")),
            last_updated=_as_string(data.get("last_updated")),
        ),
        body=body,
    )


def slug(heading: str) -> str:
    """Slugify a heading into a GitHub-style anchor."""
    s = _SLUG_STRIP_RE.sub("", heading.lower()).strip()
    return _SLUG_SPACE_RE.sub("-", s)


@dataclass(slots=True)
class _Section:
    heading: str
    text: str


def _split_sections(body: str) -> list[_Section]:
    """Group body lines into sections delimited by H1-H3 ATX headings."""
    sections: list[_Section] = []
    heading = ""
    buf: list[str] = []

    def flush() -> None:
        nonlocal buf
        text = "\n".join(buf).strip()
        if text or heading:
            sections.append(_Section(heading=heading, text=text))
        buf = []

    for line in body.split("\n"):
        m = _HEADING_RE.match(line)
        if m:
            flush()
            heading = m.group(2).strip()
        else:
            buf.append(line)
    flush()
    return [s for s in sections if s.text or s.heading]


def _window_text(text: str) -> list[str]:
    """Break an over-long section body into overlapping windows on paragraph
    boundaries where possible."""
    if len(text) <= MAX_CHARS:
        return [text]
    windows: list[str] = []
    cur = ""
    for p in _PARA_SPLIT_RE.split(text):
        if cur and len(cur) + len(p) + 2 > MAX_CHARS:
            windows.append(cur)
            cur = cur[max(0, len(cur) - OVERLAP_CHARS):]
        cur = f"{cur}\n\n{p}" if cur else p
        # A single huge paragraph: hard-split it.
        while len(cur) > MAX_CHARS:
            windows.append(cur[:MAX_CHARS])
            cur = cur[MAX_CHARS - OVERLAP_CHARS:]
    if cur.strip():
        windows.append(cur)
    return windows


def chunk_doc(markdown: str, file_path: str, release: str) -> list[IndexChunk]:
    """Produce embeddable chunks for one file. `file_path` and `release`
    identify it."""
    parsed = parse_doc(markdown)
    meta = parsed.meta
    chunks: list[IndexChunk] = []
    for section in _split_sections(parsed.body):
        for window in _window_text(section.text):
            if not window.strip() and not section.heading:
                continue
            chunks.append(
                IndexChunk(
                    path=file_path,
                    anchor=slug(section.heading) if section.heading else "",
                    title=meta.title,
                    breadcrumb=meta.breadcrumb,
                    heading=section.heading,
                    release=release,
                    content=window.strip(),
                    canonical_url=meta.canonical_url,
                    last_updated=meta.last_updated,
                )
            )
    # A file with only frontmatter / no body still gets one title-only chunk so
    # it is searchable by title.
    if not chunks and meta.title:
        chunks.append(
            IndexChunk(
                path=file_path,
                anchor="",
                title=meta.title,
                breadcrumb=meta.breadcrumb,
                heading=meta.title,
                release=release,
                content=meta.breadcrumb,
                canonical_url=meta.canonical_url,
                last_updated=meta.last_updated,
            )
        )
    return chunks


def chunk_embed_text(c: IndexChunk) -> str:
    """The text actually fed to the embedder for a chunk: context + body."""
    seen: list[str] = []
    for v in (c.breadcrumb, c.title, c.heading):
        if v and v not in seen:
            seen.append(v)
    ctx = " — ".join(seen)
    return f"{ctx}\n\n{c.content}" if ctx else c.content
