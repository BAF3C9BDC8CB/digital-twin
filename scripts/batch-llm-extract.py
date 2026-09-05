#!/usr/bin/env python3
"""
批量 LLM 知识提取：对 hermes-docs-zh 全部 markdown 文档做实体/关系/描述提取，
结果写入 hermes-docs-zh-llm/（镜像源码目录结构，.md -> .json），
随后与 hermes-docs-zh-hanlp/ 对比生成质量验证报告。

用法：
  python3 scripts/batch-llm-extract.py                 # 提取全部文档
  python3 scripts/batch-llm-extract.py --file X.md     # 只提取单篇（可断点续跑）
  python3 scripts/batch-llm-extract.py --concurrency 4 # 并发数（默认 4，勿超 4 防限流）
  python3 scripts/batch-llm-extract.py --skip-extract  # 只生成汇总+对比报告
"""

import argparse
import json
import os
import re
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path

import requests
import yaml

SRC_DIR = Path(__file__).resolve().parent.parent / "hermes-docs-zh"
OUT_DIR = Path(__file__).resolve().parent.parent / "hermes-docs-zh-llm"
HANLP_DIR = Path(__file__).resolve().parent.parent / "hermes-docs-zh-hanlp"

# 分块参数（基于实测：R1-8B 单次 8KB 输入约 100s、输出稳定）
CHUNK_CHARS = 8000       # 超过此长度分块
MAX_CHARS_PER_CALL = 8000  # 每次调用喂给 LLM 的最大字符数
TOPIC_LEN = 80           # 分块时携带的前缀（标题上下文）

ENTITY_TYPES = {"Service", "Module", "Tool", "File", "Technology", "Concept", "Platform"}
RELATION_TYPES = {"CONTAINS", "DEPENDS_ON", "USES", "IMPLEMENTS", "CALLS", "MANAGES"}

_lock = threading.Lock()
_stats = {"ok": 0, "fail": 0}
_failed_files = []


def load_config():
    cfg = yaml.safe_load(open(Path(__file__).resolve().parent.parent / "config" / "pipeline.yaml"))
    sf = cfg["providers"]["siliconflow"]
    return sf["url"].rstrip("/") + "/chat/completions", sf["api_key"], sf["model_llm"]


def split_markdown(text: str, max_chars=MAX_CHARS_PER_CALL, topic_len=TOPIC_LEN):
    """按段落切分，保证每块 <= max_chars；块首带标题行（若恰在标题后切分）保持上下文。"""
    if len(text) <= max_chars:
        return [text]
    # 按标题/段落边界粗分
    lines = text.split("\n")
    chunks = []
    cur = []
    cur_len = 0
    for line in lines:
        # 标题行优先作为新块起点
        is_heading = bool(re.match(r"^#{1,6}\s", line))
        if is_heading and cur and cur_len + len(line) > max_chars * 0.6:
            chunks.append("\n".join(cur))
            cur = [line]
            cur_len = len(line)
        elif cur_len + len(line) + 1 > max_chars and cur:
            chunks.append("\n".join(cur))
            cur = [line]
            cur_len = len(line)
        else:
            cur.append(line)
            cur_len += len(line) + 1
    if cur:
        chunks.append("\n".join(cur))
    return chunks


def build_prompt(doc_name: str, chunk: str, is_chunked: bool, chunk_idx: int, total_chunks: int) -> str:
    header = f"文档名称：{doc_name}"
    if is_chunked:
        header += f"\n（这是该文档的第 {chunk_idx}/{total_chunks} 个片段）"
    return f"""你是一个技术文档知识提取专家。请从以下文档中提取实体和关系。

{header}

提取规则：
1. 实体类型（仅使用以下类型）：
   - Service: 服务/组件（如 Gateway、AIAgent、CLI）
   - Module: 模块/包（如 agent/、tools/、gateway/）
   - Tool: 工具/命令（如 terminal、browser、mcp）
   - File: 文件（如 run_agent.py、cli.py）
   - Technology: 技术栈（如 SQLite、FTS5、Python）
   - Concept: 技术概念（如 Prompt Builder、Provider Resolution）
   - Platform: 平台（如 Telegram、Discord、Slack）

2. 关系类型（仅使用以下类型）：
   - CONTAINS: 包含关系
   - DEPENDS_ON: 依赖关系
   - USES: 使用关系
   - IMPLEMENTS: 实现关系
   - CALLS: 调用关系
   - MANAGES: 管理关系

3. 输出要求（严格遵守）：
   - 实体只提取最重要的 8-15 个，宁缺毋滥
   - summary 用一句话，不超过 20 个中文字
   - relations 只输出 4-10 条最有把握的
   - evidence 只引用原文，不超过 30 个字
   - 只提取该片段内出现的内容，不要臆造

4. 输出格式（严格 JSON，只返回 JSON 本身，不要任何解释、不要 markdown 代码块）：
{{"entities": [{{"name": "实体名称", "type": "类型", "aliases": ["别名1"], "summary": "简短描述"}}], "relations": [{{"head": "源实体", "tail": "目标实体", "type": "关系类型", "confidence": 0.9, "evidence": "原文引用"}}]}}

文档内容：
{chunk}
"""


def normalize(extracted: dict) -> dict:
    """归一化模型输出：aliases 字符串->list、补 summary、过滤非 dict/缺关键字段。"""
    ents, rels = [], []
    for e in extracted.get("entities", []):
        if not isinstance(e, dict) or not e.get("name") or not e.get("type"):
            continue
        aliases = e.get("aliases", [])
        if isinstance(aliases, str):
            aliases = [aliases] if aliases else []
        item = dict(e)
        item["aliases"] = aliases
        item.setdefault("summary", "")
        ents.append(item)
    for r in extracted.get("relations", []):
        if isinstance(r, dict) and r.get("head") and r.get("tail") and r.get("type"):
            rels.append(r)
    return {"entities": ents, "relations": rels}


def call_llm(api_url, api_key, model, prompt, timeout=300):
    resp = requests.post(
        api_url,
        headers={"Authorization": f"Bearer {api_key}", "Content-Type": "application/json"},
        json={
            "model": model,
            "messages": [
                {"role": "system", "content": "You are a helpful assistant designed to output JSON."},
                {"role": "user", "content": prompt},
            ],
            "temperature": 0.1,
            "max_tokens": 6000,
            "response_format": {"type": "json_object"},
        },
        timeout=timeout,
    )
    resp.raise_for_status()
    d = resp.json()
    content = d["choices"][0]["message"].get("content", "")
    # 剥 markdown 代码块
    if "```json" in content:
        content = content.split("```json")[1].split("```")[0]
    elif "```" in content:
        content = content.split("```")[1].split("```")[0]
    content = content.strip()
    obj = json.loads(content)
    return obj, d.get("usage", {})


def merge_results(chunks_results):
    """合并多块结果：实体按 name 去重（别名/描述合并），关系整体拼接去重。"""
    entities, rels = [], []
    seen_names = {}
    for obj in chunks_results:
        for e in obj.get("entities", []):
            name = e.get("name")
            if not name:
                continue
            if name in seen_names:
                prev = seen_names[name]
                # 合并 aliases
                for a in e.get("aliases", []):
                    if a and a not in prev.get("aliases", []):
                        prev["aliases"].append(a)
                # 补 summary（优先非空）
                if not prev.get("summary") and e.get("summary"):
                    prev["summary"] = e["summary"]
            else:
                seen_names[name] = dict(e)
        for r in obj.get("relations", []):
            rels.append(r)
    entities = list(seen_names.values())
    # 关系去重
    seen_rels = set()
    dedup_rels = []
    for r in rels:
        key = (r.get("head"), r.get("tail"), r.get("type"))
        if key in seen_rels:
            continue
        seen_rels.add(key)
        dedup_rels.append(r)
    return {"entities": entities, "relations": dedup_rels}


def extract_file(rel_path: str, api_url, api_key, model, force=False):
    """提取单篇文档，返回 (rel_path, success, result) 供写盘。"""
    src = SRC_DIR / rel_path
    text = src.read_text(encoding="utf-8")
    out_json = OUT_DIR / rel_path.replace(".md", ".json")

    if out_json.exists() and not force:
        with _lock:
            _stats["ok"] += 1
        return rel_path, True, {"skipped": True}

    doc_name = rel_path.replace(".md", "")
    chunks = split_markdown(text)

    t0 = time.time()
    chunks_results = []
    usage = {}
    try:
        is_chunked = len(chunks) > 1
        for i, chunk in enumerate(chunks, 1):
            prompt = build_prompt(doc_name, chunk, is_chunked, i, len(chunks))
            obj, u = call_llm(api_url, api_key, model, prompt)
            chunks_results.append(normalize(obj))
            for k, v in u.items():
                usage[k] = usage.get(k, 0) + (v if isinstance(v, int) else 0)
        merged = merge_results(chunks_results)
        elapsed = time.time() - t0
        with _lock:
            _stats["ok"] += 1
        return rel_path, True, {
            "source_file": rel_path,
            "model": model,
            "chunks": len(chunks),
            "num_entities": len(merged["entities"]),
            "num_relations": len(merged["relations"]),
            "processing_time_sec": round(elapsed, 2),
            "entities": merged["entities"],
            "relations": merged["relations"],
            "usage_tokens": usage,
        }
    except Exception as e:
        elapsed = time.time() - t0
        with _lock:
            _stats["fail"] += 1
            _failed_files.append(rel_path)
        return rel_path, False, {"error": str(e), "time_sec": round(elapsed, 2)}


def write_result(rel_path, ok, payload):
    if not ok:
        return
    out_json = OUT_DIR / rel_path.replace(".md", ".json")
    out_json.parent.mkdir(parents=True, exist_ok=True)
    with open(out_json, "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--file", help="只提取指定相对路径 md（如 developer-guide/architecture.md）")
    ap.add_argument("--concurrency", type=int, default=4, help="并发数（默认 4，勿超过防限流）")
    ap.add_argument("--force", action="store_true", help="强制重跑已存在文件")
    ap.add_argument("--skip-extract", action="store_true", help="跳过提取，只生成汇总+对比报告")
    args = ap.parse_args()

    api_url, api_key, model = load_config()

    if args.skip_extract:
        print(f"模型: {model}")
    else:
        print(f"模型: {model} | 并发: {args.concurrency}")
        if args.file:
            md_files = [args.file]
        else:
            md_files = sorted(str(p.relative_to(SRC_DIR)) for p in SRC_DIR.rglob("*.md"))
        print(f"待处理文档: {len(md_files)} 篇")

        with ThreadPoolExecutor(max_workers=args.concurrency) as ex:
            futures = {ex.submit(extract_file, f, api_url, api_key, model, args.force): f for f in md_files}
            done = 0
            for fut in as_completed(futures):
                rel, ok, payload = fut.result()
                write_result(rel, ok, payload)
                done += 1
                if ok and not payload.get("skipped"):
                    status = f"✓ {rel}: {payload.get('num_entities', '?')}实体/{payload.get('num_relations', '?')}关系 {payload.get('processing_time_sec', '?')}s"
                elif ok:
                    status = f"• {rel}: 已存在跳过"
                else:
                    status = f"✗ {rel}: {payload.get('error', '')[:80]}"
                print(f"[{done}/{len(md_files)}] {status}", flush=True)

        if _failed_files:
            print("\n失败文件:")
            for f in _failed_files:
                print("  -", f)
            print(f"\n成功 {_stats['ok']} / 失败 {_stats['fail']}")

    # 汇总 + 对比报告
    build_reports(model)


def build_reports(model):
    """汇总全部 json -> summary.json，并生成与 HanLP 的对比报告。"""
    print("\n== 生成汇总与对比报告 ==")
    files = sorted(str(p.relative_to(OUT_DIR)) for p in OUT_DIR.rglob("*.json"))
    per_file = []
    total_ent = total_rel = 0
    for rel in files:
        d = json.load(open(OUT_DIR / rel, encoding="utf-8"))
        n_ent = d.get("num_entities", len(d.get("entities", [])))
        n_rel = d.get("num_relations", len(d.get("relations", [])))
        total_ent += n_ent
        total_rel += n_rel
        per_file.append({
            "file": rel.replace(".json", ".md"),
            "entities": n_ent,
            "relations": n_rel,
            "time_sec": d.get("processing_time_sec", 0),
            "chunks": d.get("chunks", 1),
        })
    summary = {
        "src_dir": str(SRC_DIR),
        "out_dir": str(OUT_DIR),
        "model": model,
        "files": per_file,
        "totals": {"files": len(per_file), "entities": total_ent, "relations": total_rel},
    }
    with open(OUT_DIR / "summary.json", "w", encoding="utf-8") as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)
    print(f"汇总: {len(per_file)} 篇, {total_ent} 实体, {total_rel} 关系 -> {OUT_DIR / 'summary.json'}")

    # 对比 hanlp
    hanlp_summary = json.load(open(HANLP_DIR / "summary.json", encoding="utf-8")) if (HANLP_DIR / "summary.json").exists() else None
    print(f"HanLP 汇总存在: {hanlp_summary is not None}")
    if hanlp_summary:
        write_comparison_report(per_file, hanlp_summary, model)


def write_comparison_report(llm_files, hanlp_summary, model):
    hanlp_by_file = {f["file"]: f for f in hanlp_summary["files"]}
    llm_by_file = {f["file"]: f for f in llm_files}
    all_files = sorted(set(hanlp_by_file) | set(llm_by_file))

    rows = []
    for f in all_files:
        h = hanlp_by_file.get(f)
        l = llm_by_file.get(f)
        rows.append({
            "file": f,
            "hanlp_entities": h["entities"] if h else "-",
            "hanlp_relations": h["relations"] if h else "-",
            "llm_entities": l["entities"] if l else "-",
            "llm_relations": l["relations"] if l else "-",
        })

    h_tot_ent = hanlp_summary.get("totals", {}).get("entities", sum(r["entities"] for r in hanlp_summary["files"]))
    h_tot_rel = hanlp_summary.get("totals", {}).get("relations", sum(r["relations"] for r in hanlp_summary["files"]))
    l_tot_ent = sum(r["entities"] for r in llm_files)
    l_tot_rel = sum(r["relations"] for r in llm_files)
    n = len(all_files)

    lines = []
    lines.append("# Hermes 文档 LLM 知识提取质量对比报告\n")
    lines.append(f"生成时间: 2026-09-05")
    lines.append(f"提取模型: {model}\n")
    lines.append(f"覆盖文档: {n} 篇（hermes-docs-zh 全部）\n")
    lines.append("## 总量对比\n")
    lines.append(f"| 维度 | HanLP | LLM (R1-8B) |")
    lines.append(f"|------|-------|-------------|")
    lines.append(f"| 实体总数 | {h_tot_ent} | {l_tot_ent} |")
    lines.append(f"| 关系总数 | {h_tot_rel} | {l_tot_rel} |")
    lines.append(f"| 平均实体/篇 | {h_tot_ent/n:.1f} | {l_tot_ent/n:.1f} |")
    lines.append(f"| 平均关系/篇 | {h_tot_rel/n:.1f} | {l_tot_rel/n:.1f} |")
    lines.append("")
    lines.append("## 分文件对比\n")
    lines.append("| 文档 | HanLP 实体 | HanLP 关系 | LLM 实体 | LLM 关系 |")
    lines.append("|------|-----------|-----------|---------|---------|")
    for r in rows:
        lines.append(f"| {r['file']} | {r['hanlp_entities']} | {r['hanlp_relations']} | {r['llm_entities']} | {r['llm_relations']} |")

    out = OUT_DIR / "QUALITY_COMPARISON_REPORT.md"
    with open(out, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"对比报告: {out}")


if __name__ == "__main__":
    main()
