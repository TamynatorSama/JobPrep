"""Token meter readout.

  GET /metrics/usage  -> {since, rows: [{feature, model, calls, input_tokens,
                          cached_input_tokens, output_tokens}]}

Totals since sidecar start, per app feature and model (see the token meter in
llm_provider.py). Token-guarded like the other bridge endpoints: the sidecar
allows any origin, so an unguarded endpoint would let any web page read usage.
The latency harness (bench/latency_harness.py) reads it before and after a run.
"""
from fastapi import APIRouter, Depends

import llm_provider as llm_factory
from routes.bridge import require_token

router = APIRouter()


@router.get("/usage")
async def usage(_: None = Depends(require_token)):
    return llm_factory.usage_snapshot()
