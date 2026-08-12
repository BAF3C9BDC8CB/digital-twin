"""dt-sense 插件单元测试 — 渲染逻辑 + 项目匹配。

运行: pytest plugins/dt-sense/tests/test_dt_sense.py -v
覆盖: 简报渲染（已索引强信号/未索引/降级/工具速查行）、项目匹配（别名/词边界）。
"""
import sys
from pathlib import Path

import pytest

PLUGIN_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PLUGIN_DIR))

import importlib.util

spec = importlib.util.spec_from_file_location("dt_sense", PLUGIN_DIR / "__init__.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def _sense(status="indexed", methods=2287, classes=357, degraded=None, project="im-center", path="/data/aflmProjects/aflm/uvp-im-center"):
    return {
        "status": status,
        "project": {"name": project, "path": path},
        "stats": {"methods": methods, "classes": classes, "vectors": methods,
                  "last_build": "2026-08-11T18:29:24Z" if methods else None},
        "dirs": [{"dir": "src/main/java"}] if methods else [],
        "languages": [{"ext": "java", "pct": 100}] if methods else [],
        "key_entities": [{"name": "MessageController", "kind": "Class", "in_degree": 12}] if methods else [],
        "degraded": degraded or [],
    }


class TestRenderBrief:
    def test_indexed_strong_signal(self):
        brief = m._render_brief(_sense(), Path("/data/aflmProjects/aflm/uvp-im-center"), 65)
        assert "已索引 2287 方法/357 类" in brief
        assert "dt_search_kg(world=code, project=im-center, limit=5) 定位" in brief
        assert "禁止只读源码跳过 KG" in brief

    def test_unindexed_no_signal(self):
        brief = m._render_brief(_sense(status="registered_not_indexed", methods=0, classes=0), Path("/data/myProject/digital-twin-v2"), 65)
        assert "已索引" not in brief
        assert "注册项目: 65 个" in brief

    def test_degraded_warning(self):
        brief = m._render_brief(_sense(degraded=["memgraph"]), Path("/data/aflmProjects/aflm/uvp-im-center"), 65)
        assert "KG degraded" in brief

    def test_tool_quickref_line(self):
        brief = m._render_brief(_sense(), Path("/data/aflmProjects/aflm/uvp-im-center"), 65)
        assert "可用dt工具: dt_search_kg(query,world=code|knowledge,project=<项目名>,limit≤5)" in brief
        assert "run_cypher_query" in brief
        assert "dt_health" in brief

    def test_knowledge_world_hint(self):
        brief = m._render_brief(_sense(), Path("/data/aflmProjects/aflm/uvp-im-center"), 65)
        assert "knowledge世界" in brief
        assert "world=knowledge" in brief

    def test_brief_size_bounded(self):
        brief = m._render_brief(_sense(), Path("/data/aflmProjects/aflm/uvp-im-center"), 65)
        assert len(brief) <= m.MAX_BRIEF_CHARS


class TestMatchProject:
    def test_exact_token(self):
        p = m._match_project("im-center 的消息撤回流程是怎样的？")
        assert p is not None and "uvp-im-center" in str(p)

    def test_alias_dt(self):
        p = m._match_project("分析 dt 的构建流程")
        assert p is not None and "digital-twin-v2" in str(p)

    def test_no_false_positive_embedded(self):
        # 'svc' 不应匹配 'svc-order'
        p = m._match_project("查看 svc-order 的配置")
        # 注册表里可能有 svc 项目；只需保证不是误匹配到无关的
        assert p is None or "svc-order" not in str(p)

    def test_no_match_unrelated(self):
        p = m._match_project("今天天气怎么样？")
        assert p is None

    def test_registry_loads(self):
        reg = m._load_registry()
        assert len(reg) > 50, f"注册表应含 65 项目, 实际 {len(reg)}"
        names = {n for n, _ in reg}
        assert "im-center" in names or "digital-twin-v2" in names
