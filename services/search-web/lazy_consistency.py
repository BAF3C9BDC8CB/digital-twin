"""
Lazy Consistency Checker for Digital Twin search-web.
Performs best-effort consistency verification between Neo4j and Qdrant,
and discovers unindexed files.
"""

import os
import json
import subprocess
from typing import List, Tuple, Optional

import requests

NEO4J_URL = os.environ.get("NEO4J_URL", "http://localhost:7474/db/neo4j/tx/commit")
NEO4J_AUTH = os.environ.get("NEO4J_AUTH", "")


def _neo4j_query(cypher: str, params: dict = None) -> list:
    """Run a Cypher query and return rows."""
    if not NEO4J_AUTH:
        return []
    try:
        r = requests.post(
            NEO4J_URL,
            json={"statements": [{"statement": cypher, "parameters": params or {}}]},
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Basic {NEO4J_AUTH}",
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
    except Exception:
        return []


class ConsistencyChecker:
    """Check consistency between Neo4j and Qdrant, discover new files."""

    def _resolve_project_root(self, project: str) -> Optional[str]:
        """Get project root path from Neo4j."""
        rows = _neo4j_query(
            "MATCH (p:Project {name: $name}) RETURN p.path",
            {"name": project},
        )
        if rows and rows[0]:
            return rows[0][0]
        return None

    def verify_and_repair(self, vector: list, results: list) -> Tuple[list, dict]:
        """Verify results and return (results, stats).
        
        Checks if returned methods exist in Neo4j and marks stale entries.
        """
        stats = {"dirty_files": 0, "verified": 0, "stale": 0}
        if not results or not NEO4J_AUTH:
            return results, stats

        method_ids = [
            r.get("payload", {}).get("method_id", "").strip('"')
            for r in results
        ]
        method_ids = [m for m in method_ids if m]

        if not method_ids:
            return results, stats

        verified_rows = _neo4j_query(
            "MATCH (m:Method) WHERE m.method_id IN $ids RETURN m.method_id, m.file_path",
            {"ids": method_ids},
        )
        verified_ids = {row[0] for row in verified_rows}

        for r in results:
            mid = r.get("payload", {}).get("method_id", "").strip('"')
            if mid and mid in verified_ids:
                stats["verified"] += 1
            elif mid:
                stats["stale"] += 1

        return results, stats

    def discover_new_files(self, project: str) -> List[Tuple[str, str, str]]:
        """Discover files that exist on disk but are not indexed.
        
        Returns list of (project_name, file_path, reason).
        """
        root = self._resolve_project_root(project)
        if not root or not os.path.isdir(root):
            return []

        new_files = []
        try:
            result = subprocess.run(
                ["dt", "validate", "--path", root, "--name", project],
                capture_output=True, text=True, timeout=60,
            )
            for line in result.stdout.splitlines():
                if "skip" in line.lower() or "error" in line.lower():
                    parts = line.split()
                    for part in parts:
                        if os.path.exists(os.path.join(root, part)):
                            new_files.append((project, part, "unindexed"))
        except Exception:
            pass

        return new_files
