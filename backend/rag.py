"""Lightweight retrieval over a job's own corpus — the candidate's resume,
the company-research dossier, and prior chat threads for the same job.

Design constraints (see CLAUDE.md):
  * The corpus per job is tiny (one resume, ~one research doc, a handful of
    chats), so a full vector DB (FAISS/Chroma) is overkill. We embed chunks
    with Gemini's `text-embedding-004` (same API key the chat already uses)
    and rank with plain-Python cosine similarity. No numpy / faiss dependency.
  * Retrieval is stateless from the frontend's point of view: the client
    sends the job's documents with every chat request. To avoid re-embedding
    unchanged text on each message, chunk embeddings are cached on disk,
    keyed by a content hash — so only new/edited chunks cost an embed call.
  * RAG is strictly best-effort. Any failure here must degrade to "no
    retrieved context", never break the chat stream. Callers wrap in try.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import threading
from pathlib import Path
from typing import List, Sequence, Tuple

from langchain_google_genai import GoogleGenerativeAIEmbeddings

EMBED_MODEL = "models/text-embedding-004"

# Chunking: pack paragraph-ish spans into windows of roughly this many chars.
# Big enough to keep a resume bullet or a research paragraph intact; small
# enough that top-k retrieval stays focused.
CHUNK_CHARS = 900
CHUNK_OVERLAP = 150

# Retrieval defaults — top-k chunks, capped total chars so the system prompt
# stays within a sane token budget alongside the existing job_context.
DEFAULT_K = 6
DEFAULT_MAX_CHARS = 6000

_cache_lock = threading.Lock()


# --------------------------------------------------------------------------- #
# Disk cache (content-addressed embeddings)
# --------------------------------------------------------------------------- #
def _cache_path() -> Path:
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or "."
    d = Path(base) / "InterPrep" / "rag_cache"
    d.mkdir(parents=True, exist_ok=True)
    return d / "embeddings.json"


def _hash(text: str) -> str:
    return hashlib.sha1(text.encode("utf-8")).hexdigest()


def _load_cache() -> dict:
    path = _cache_path()
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, ValueError, OSError):
        return {}


def _save_cache(cache: dict) -> None:
    path = _cache_path()
    tmp = path.with_suffix(".json.tmp")
    try:
        tmp.write_text(json.dumps(cache), encoding="utf-8")
        os.replace(tmp, path)  # atomic on Windows
    except OSError:
        pass  # cache is an optimization; losing a write is harmless


# --------------------------------------------------------------------------- #
# Chunking
# --------------------------------------------------------------------------- #
def _chunk(text: str) -> List[str]:
    text = (text or "").strip()
    if not text:
        return []
    # Split on blank lines first so we don't cut mid-paragraph, then greedily
    # pack paragraphs into windows. Oversized paragraphs are hard-split.
    paras = [p.strip() for p in re.split(r"\n\s*\n", text) if p.strip()]
    chunks: List[str] = []
    buf = ""
    for p in paras:
        if len(p) > CHUNK_CHARS:
            if buf:
                chunks.append(buf)
                buf = ""
            for i in range(0, len(p), CHUNK_CHARS - CHUNK_OVERLAP):
                chunks.append(p[i : i + CHUNK_CHARS])
            continue
        if not buf:
            buf = p
        elif len(buf) + 2 + len(p) <= CHUNK_CHARS:
            buf += "\n\n" + p
        else:
            chunks.append(buf)
            buf = p
    if buf:
        chunks.append(buf)
    return chunks


# --------------------------------------------------------------------------- #
# Embedding (cached) + cosine
# --------------------------------------------------------------------------- #
def _embed_cached(emb: GoogleGenerativeAIEmbeddings, texts: Sequence[str]) -> List[List[float]]:
    with _cache_lock:
        cache = _load_cache()
        missing = sorted({t for t in texts if _hash(t) not in cache})
        if missing:
            vectors = emb.embed_documents(list(missing))
            for t, v in zip(missing, vectors):
                cache[_hash(t)] = v
            _save_cache(cache)
        return [cache[_hash(t)] for t in texts]


def _cosine(a: Sequence[float], b: Sequence[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    if na == 0 or nb == 0:
        return 0.0
    return dot / (na * nb)


# --------------------------------------------------------------------------- #
# Public entry point
# --------------------------------------------------------------------------- #
def build_context(
    query: str,
    documents: Sequence,  # objects with .source and .text (models.RagDoc)
    api_key: str,
    k: int = DEFAULT_K,
    max_chars: int = DEFAULT_MAX_CHARS,
) -> str:
    """Return a formatted "Retrieved context" block for the most relevant
    chunks across `documents`, or "" when there's nothing useful to add.

    Synchronous and network-bound (embedding calls) — call via
    `asyncio.to_thread(...)` from async handlers.
    """
    if not query or not query.strip() or not documents or not api_key:
        return ""

    # (source, chunk_text) for every chunk across all docs.
    chunks: List[Tuple[str, str]] = []
    for doc in documents:
        for c in _chunk(getattr(doc, "text", "")):
            chunks.append((getattr(doc, "source", "context"), c))
    if not chunks:
        return ""

    emb = GoogleGenerativeAIEmbeddings(model=EMBED_MODEL, google_api_key=api_key)
    vecs = _embed_cached(emb, [c[1] for c in chunks])
    qv = emb.embed_query(query)

    scored = sorted(
        (( _cosine(qv, v), src, txt) for v, (src, txt) in zip(vecs, chunks)),
        key=lambda t: t[0],
        reverse=True,
    )

    selected: List[Tuple[str, str]] = []
    total = 0
    for score, src, txt in scored[: max(k, 0)]:
        if score <= 0:
            continue
        if total + len(txt) > max_chars and selected:
            break
        selected.append((src, txt))
        total += len(txt)
    if not selected:
        return ""

    blocks = [f"[{src}]\n{txt}" for src, txt in selected]
    return (
        "**Retrieved context** — relevant excerpts from the candidate's resume, "
        "company research, and prior chats for this job. Use it to ground your "
        "answer; cite specifics where helpful.\n\n" + "\n\n---\n\n".join(blocks)
    )
