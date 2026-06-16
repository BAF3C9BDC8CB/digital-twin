import os
import sys
import json
from urllib.parse import quote

import requests
import uvicorn
from fastapi import FastAPI, Request, Query
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

# Lazy Consistency Engine
sys.path.insert(0, "/data/myProject/digital-twin/engine")
from lazy_consistency import ConsistencyChecker
_consistency_checker = None

def get_consistency_checker():
    global _consistency_checker
    if _consistency_checker is None:
        _consistency_checker = ConsistencyChecker()
    return _consistency_checker

EMBED_URL = os.environ.get("EMBED_URL", "http://localhost:8001")
QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
NEO4J_URL = os.environ.get("NEO4J_URL", "http://localhost:7474/db/neo4j/tx/commit")

app = FastAPI(title="Code Search")
templates = Jinja2Templates(directory=os.path.join(os.path.dirname(__file__), "templates"))


def list_projects():
    """从 Neo4j 获取所有项目名，用于下拉框"""
    auth = os.environ.get("NEO4J_AUTH", "")
    if not auth:
        return []
    try:
        r = requests.post(
            NEO4J_URL,
            json={"statements": [{"statement": "MATCH (p:Project) RETURN p.name ORDER BY p.name"}]},
            headers={"Content-Type": "application/json", "Authorization": f"Basic {auth}"},
            timeout=10,
        )
        rows = r.json()["results"][0]["data"]
        return [row["row"][0] for row in rows]
    except Exception:
        return []

def list_domains():
    """返回可搜索的领域: code, document, environment"""
    return ["code", "document", "environment"]


def embed(text: str):
    r = requests.post(f"{EMBED_URL}/embed", json={"text": text}, timeout=120)
    r.raise_for_status()
    return r.json()["vector"]


def list_qdrant_code_collections():
    """列出 Qdrant 中所有以 _methods 结尾的代码集合"""
    try:
        r = requests.get(f"{QDRANT_URL}/collections", timeout=10)
        if r.status_code == 200:
            cols = r.json()["result"]["collections"]
            return [c["name"] for c in cols if c["name"].endswith("_methods")]
    except Exception:
        pass
    return []


def search_qdrant(collection: str, vector: list, limit: int, project: str = ""):
    """搜索 Qdrant，支持 project 过滤"""
    payload_filter = None
    if project:
        payload_filter = {
            "must": [{"key": "project", "match": {"value": project}}]
        }
    body = {
        "vector": vector,
        "limit": limit,
        "with_payload": True,
        "with_vector": False,
    }
    if payload_filter:
        body["filter"] = payload_filter

    try:
        r = requests.post(
            f"{QDRANT_URL}/collections/{collection}/points/search",
            json=body,
            timeout=30,
        )
        if r.status_code == 200:
            return r.json()["result"]
    except Exception:
        pass
    return []


def search_code_all(vector: list, limit: int, project: str = ""):
    """跨所有项目搜索代码（每个项目独立的 _methods 集合）"""
    if project:
        col = f"{project}_methods"
        return search_qdrant(col, vector, limit, project)

    all_results = []
    for col in list_qdrant_code_collections():
        results = search_qdrant(col, vector, limit)
        all_results.extend(results)
        if len(all_results) >= limit * 3:
            break

    # Sort by score and truncate
    all_results.sort(key=lambda x: x.get("score", 0), reverse=True)
    return all_results[:limit]


def neo4j_query(cypher: str, params: dict = None):
    auth = os.environ.get("NEO4J_AUTH", "")
    if not auth:
        return []
    r = requests.post(
        NEO4J_URL,
        json={"statements": [{"statement": cypher, "parameters": params or {}}]},
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Basic {auth}",
        },
        timeout=10,
    )
    if r.status_code != 200:
        return []
    data = r.json()
    results = []
    for row in data.get("results", []):
        for entry in row.get("data", []):
            results.append(entry["row"])
    return results


def expand_method(method_id: str):
    if not method_id:
        return [], []
    callers = neo4j_query(
        "MATCH (caller:Method)-[:CALLS]->(m:Method {method_id: $mid}) "
        "RETURN caller.name, caller.file_path, caller.start_line ORDER BY caller.file_path, caller.name LIMIT 500",
        {"mid": method_id},
    )
    callees = neo4j_query(
        "MATCH (m:Method {method_id: $mid})-[:CALLS]->(callee:Method) "
        "RETURN callee.name, callee.file_path, callee.start_line ORDER BY callee.file_path, callee.name LIMIT 500",
        {"mid": method_id},
    )
    return callers, callees


def search_all(vector: list, limit: int, domain: str = "code"):
    """跨项目搜索指定域"""
    if domain == "code":
        return search_code_all(vector, limit)
    elif domain == "document":
        return search_qdrant("document", vector, limit)
    elif domain == "environment":
        return search_qdrant("environment", vector, limit)
    return []


LANG_MAP = {
    "java": "java", "javascript": "javascript", "typescript": "typescript",
    "vue": "html", "xml": "xml", "yaml": "yaml", "json": "json",
    "sql": "sql", "python": "python", "css": "css", "scss": "scss",
    "markdown": "markdown", "php": "php",
}


def highlight_lang(payload: dict) -> str:
    lang = (payload.get("language") or "").lower()
    return LANG_MAP.get(lang, "plaintext")


@app.get("/", response_class=HTMLResponse)
def index(request: Request):
    projects = list_projects()
    domains = list_domains()
    return templates.TemplateResponse(request, "index.html", {
        "collections": projects,
        "domains": domains,
        "query": "",
        "project": "",
        "domain": "code",
        "results": None,
    })


@app.get("/search", response_class=HTMLResponse)
def search(
    request: Request,
    q: str = Query(...),
    project: str = Query(""),
    domain: str = Query("code"),
    limit: int = Query(10),
):
    projects = list_projects()
    domains = list_domains()
    query_text = q.strip()
    if not query_text:
        return templates.TemplateResponse(request, "index.html", {
            "collections": projects,
            "domains": domains,
            "query": "",
            "project": project,
            "domain": domain,
            "results": [],
            "error": "请输入搜索内容",
        })

    try:
        vec = embed(query_text)
    except Exception as e:
        return templates.TemplateResponse(request, "index.html", {
            "collections": projects,
            "domains": domains,
            "query": query_text,
            "project": project,
            "domain": domain,
            "results": [],
            "error": f"嵌入服务出错: {e}",
        })

    try:
        if domain == "code":
            results = search_code_all(vec, limit, project)
        else:
            collection = {
                "document": "document",
                "environment": "environment",
            }.get(domain)
            if collection:
                results = search_qdrant(collection, vec, limit)
            else:
                results = []

        # === Lazy Consistency Check ===
        if domain == "code" and results:
            try:
                checker = get_consistency_checker()
                results, stats = checker.verify_and_repair(vec, results)

                # 结果太少? 可能新文件未索引 — 轻量发现 + 自动索引
                if len(results) < min(5, limit) and project:
                    import subprocess as _sp
                    new_files = checker.discover_new_files(project)
                    if new_files:
                        proj_root = checker._resolve_project_root(project)
                        for proj, fpath, _ in new_files[:10]:
                            _sp.Popen(["dt", "update", "--path", proj_root or "", "--name", proj, "--file", fpath],
                                     stdout=_sp.DEVNULL, stderr=_sp.DEVNULL)
                        results = search_qdrant(collection, vec, limit, project)

                if stats.get("dirty_files", 0) > 0:
                    results = search_qdrant(collection, vec, limit, project)
            except Exception as e:
                pass  # consistency check is best-effort

        for r2 in results:
            r2["payload"]["_project"] = r2["payload"].get("project", "")
    except Exception as e:
        return templates.TemplateResponse(request, "index.html", {
            "collections": projects,
            "domains": domains,
            "query": query_text,
            "project": project,
            "domain": domain,
            "results": [],
            "error": f"搜索出错: {e}",
        })

    for r in results:
        p = r.get("payload", {})
        p["_hl_lang"] = highlight_lang(p)

        # Preview text: source_code for code, summary/title for document, name for environment
        if p.get("source_code"):
            p["_preview"] = p["source_code"]
        elif p.get("summary"):
            p["_preview"] = p["summary"]
        elif p.get("title"):
            p["_preview"] = p["title"]
        elif p.get("name"):
            p["_preview"] = p["name"]
        else:
            p["_preview"] = ""

        p["_file_url"] = ""
        if p.get("file_path"):
            p["_file_url"] = f"/file?path={quote(p.get('file_path', ''))}&project={quote(p.get('_project', ''))}"
        elif p.get("entity_type") and p.get("entity_id"):
            p["_file_url"] = f"#entity-{p['entity_type']}-{p['entity_id']}"

        # Expand with Neo4j callers/callees if method_id exists
        method_id = p.get("method_id", "").strip('"')
        if method_id:
            try:
                callers, callees = expand_method(method_id)
                p["_callers"] = [{"name": c[0], "file": c[1], "line": c[2]} for c in callers]
                p["_callees"] = [{"name": c[0], "file": c[1], "line": c[2]} for c in callees]
            except Exception:
                p["_callers"] = []
                p["_callees"] = []
        else:
            p["_callers"] = []
            p["_callees"] = []

    return templates.TemplateResponse(request, "index.html", {
        "collections": projects,
        "domains": domains,
        "query": query_text,
        "project": project,
        "domain": domain,
        "limit": limit,
        "results": results,
        "error": None,
    })


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=3001, log_level="info")
