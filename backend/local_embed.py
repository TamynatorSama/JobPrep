"""On-device text embeddings for RAG — bge-small-en-v1.5 (int8 ONNX) on the CPU.

Retrieval runs on every coach / mock-interview message, so it shouldn't cost a
provider round-trip, and it shouldn't depend on a provider at all: Gemini's
text-embedding-004 was retired (404) and silently switched RAG off, and the
ChatGPT plan has no embeddings API. bge-small (33M params, 384-d) embeds a
query in ~3 ms and a 900-char chunk in ~25 ms here; the job corpus is a few
dozen chunks, embedded once and cached on disk by rag.py.

The model (34 MB) is fetched once from Hugging Face into the standard HF cache,
like faster-whisper's models. Imports are lazy (onnxruntime + tokenizers +
huggingface_hub take ~4 s cold) — keep them off module level.
"""
from __future__ import annotations

import threading
from typing import List, Sequence

REPO = "Xenova/bge-small-en-v1.5"
MODEL_FILE = "onnx/model_quantized.onnx"
NAMESPACE = "local:bge-small-en-v1.5-q"
# bge v1.5's retrieval instruction: prefixed to queries only, not passages.
QUERY_PREFIX = "Represent this sentence for searching relevant passages: "
MAX_TOKENS = 512
BATCH = 16

_lock = threading.Lock()
_model = None  # (session, tokenizer, needs_token_type_ids)


def _cached(name: str) -> str | None:
    from huggingface_hub import try_to_load_from_cache
    hit = try_to_load_from_cache(REPO, name)
    return hit if isinstance(hit, str) else None


def _file(name: str) -> str:
    """Local path of a model file — from the HF cache without touching the
    network (hf_hub_download re-checks the hub on every call), else download."""
    from huggingface_hub import hf_hub_download
    return _cached(name) or hf_hub_download(REPO, name)


def _load():
    global _model
    with _lock:
        if _model is None:
            import onnxruntime as ort
            from tokenizers import Tokenizer

            tok = Tokenizer.from_file(_file("tokenizer.json"))
            tok.enable_truncation(MAX_TOKENS)
            tok.enable_padding()
            opts = ort.SessionOptions()
            opts.intra_op_num_threads = 4
            # CPU on purpose: the GPU belongs to the copilot's Whisper decodes.
            sess = ort.InferenceSession(_file(MODEL_FILE), opts,
                                        providers=["CPUExecutionProvider"])
            needs_tt = any(i.name == "token_type_ids" for i in sess.get_inputs())
            _model = (sess, tok, needs_tt)
    return _model


def _embed(texts: Sequence[str]) -> List[List[float]]:
    import numpy as np

    sess, tok, needs_tt = _load()
    # Sort by length so each batch pads to similar lengths, then restore order.
    order = sorted(range(len(texts)), key=lambda i: len(texts[i]))
    vecs: dict[int, list] = {}
    for start in range(0, len(order), BATCH):
        idx = order[start:start + BATCH]
        enc = tok.encode_batch([texts[i] for i in idx])
        ids = np.array([e.ids for e in enc], dtype=np.int64)
        feeds = {"input_ids": ids,
                 "attention_mask": np.array([e.attention_mask for e in enc], dtype=np.int64)}
        if needs_tt:
            feeds["token_type_ids"] = np.zeros_like(ids)
        cls = sess.run(None, feeds)[0][:, 0]  # bge pools on the [CLS] token
        cls = cls / np.maximum(np.linalg.norm(cls, axis=1, keepdims=True), 1e-12)
        for i, v in zip(idx, cls):
            vecs[i] = v.astype(float).tolist()
    return [vecs[i] for i in range(len(texts))]


class LocalEmbeddings:
    """The two methods rag.py uses from LangChain's `Embeddings` interface."""

    def embed_documents(self, texts: List[str]) -> List[List[float]]:
        return _embed(list(texts)) if texts else []

    def embed_query(self, text: str) -> List[float]:
        return _embed([QUERY_PREFIX + text])[0]


def warm_if_cached() -> None:
    """Load the model now if it's already downloaded — called in the background
    at sidecar start, so the first coach message doesn't pay ~1 s of session
    setup. Never downloads (no network at boot)."""
    if _cached("tokenizer.json") and _cached(MODEL_FILE):
        _load()


def make() -> tuple[LocalEmbeddings, str]:
    """(embeddings, cache namespace). Loads the model now so a missing download
    fails here — where make_embeddings can fall back to a provider — rather
    than mid-retrieval."""
    _load()
    return LocalEmbeddings(), NAMESPACE
