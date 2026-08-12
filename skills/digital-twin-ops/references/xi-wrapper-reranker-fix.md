# xi wrapper script — reranker n_gpu pitfall (2026-08-07)

## Where

`/usr/local/bin/xi` — interactive menu + CLI wrapper for the xinference systemd service.
Commands: `xi start|stop|restart|deploy [all|qwen3|embed|reranker]|logs|top`.
Menu options: 4=Qwen3.5-4B, 5=BGE-M3, 6=BGE-Reranker, 7=all models, 8=stop all, 9=logs, t=GPU, o=web UI.

## The bug

`deploy_reranker()` shipped this payload:

```json
{
  "model_uid": "bge-reranker-v2-m3",
  "model_name": "bge-reranker-v2-m3",
  "model_format": "pytorch",
  "quantization": "none",
  "model_engine": "sentence_transformers",
  "model_type": "rerank",
  "n_gpu": 1          ← THIS breaks startup
}
```

Symptom: `xi deploy reranker` (or menu 6) fails to start the model, while the SAME
parameters without `n_gpu` work fine. The healthy long-running instance on this box is
CPU: `accelerators: []` in `/v1/models/instances?model_name=bge-reranker-v2-m3`.

## The fix (applied 2026-08-07, backup at /usr/local/bin/xi.bak)

Delete the `n_gpu` line → `model_type: "rerank"` is the last field. CPU mode is the
working default for reranker on this machine.

```json
{
  "model_uid": "bge-reranker-v2-m3",
  "model_name": "bge-reranker-v2-m3",
  "model_format": "pytorch",
  "quantization": "none",
  "model_engine": "sentence_transformers",
  "model_type": "rerank"
}
```

## Verification

1. `bash -n /usr/local/bin/xi` — syntax OK.
2. Extract the JSON payload, `json.loads` it — valid.
3. Deploy and check the instance: `curl -s "http://localhost:9997/v1/models/instances?model_name=bge-reranker-v2-m3"`
   → `status: READY`, `accelerators: []` (empty = CPU). Launch exit code is NOT proof —
   always check `/v1/models` or the instances endpoint ~25-30s later.

## Nuance vs the digital-twin-ops SKILL.md note

The older SKILL.md note said "do NOT pass model_engine" for the reranker (the engine
`transformers` was rejected). That is about the *value* of model_engine — the WORKING
config here passes `model_engine: sentence_transformers` (a valid value). The actual
showstopper was `n_gpu` (GPU mode), not model_engine. If a reranker deploy fails, check
for a stray `n_gpu` field first.
