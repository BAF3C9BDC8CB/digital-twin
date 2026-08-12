# Read-only `config_chunks` audit recipe

Use this recipe when auditing search/index consistency without changing Qdrant, Memgraph, or SQLite.

## Scope

Audit the Qdrant `config_chunks` collection and the config-world search path. Do not run `dt build`, `dt kg-sync`, sync commands, upserts, deletes, payload updates, or write Cypher.

## Collection and payload audit

```python
import json, urllib.request

base = "http://127.0.0.1:6333"
meta = json.load(urllib.request.urlopen(base + "/collections/config_chunks"))
print(meta["result"]["points_count"])

url = base + "/collections/config_chunks/points/scroll"
offset = None
rows = []
while True:
    body = {"limit": 256, "with_payload": True, "with_vector": False}
    if offset is not None:
        body["offset"] = offset
    req = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    result = json.load(urllib.request.urlopen(req))["result"]
    rows.extend(result["points"])
    offset = result.get("next_page_offset")
    if offset is None:
        break
```

For every point, inspect `payload` and count values that are neither `None` nor empty for:

- `source`
- `doc_id`
- `data_id`
- `namespace`
- `group`
- `llm_analysis`

Also count all payload keys and the value distribution of `source`. Keep `points_count` from collection metadata and the number of rows actually scrolled; a mismatch indicates an incomplete traversal or a changing collection.

## Search verification

Run the unified search tool/CLI with `world=config`:

1. Known positive: use a term expected in the indexed corpus, such as a service/config keyword. Prefer a query that should match an actual config value rather than a generic word.
2. Random negative: use a fresh high-entropy token that is extremely unlikely to exist, e.g. `xqzv-9f3k-no-such-service-<date>`.

Record `total`, hit ids, `entity_type`, `source_ref`, `data_id`, `namespace`, `group`, score, and `llm_analysis`. A zero-result negative is expected. Do not call a positive query “good” solely because it returns a hit: low scores should be reported as a separate retrieval-quality warning.

## Interpretation

- `data_id` may be complete while provenance is not: report each field independently.
- `namespace` and `group` can be nearly complete but still have legacy gaps.
- `source`/`doc_id` coverage must be measured from payloads, not reconstructed from `source_ref`.
- `llm_analysis=0%` means indexed analysis is absent; it does not imply search itself should invoke an LLM.
- A search result can expose metadata through a projection even when raw payload coverage is poor; always include both views in the report.

## Expected report shape

Use a compact table with `field | filled | total | percentage`, followed by source distribution, positive-query evidence, negative-query evidence, write/build exclusions, and any low-score observation.
