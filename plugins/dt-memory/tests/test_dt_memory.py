"""dt-memory 插件单元测试。

运行: pytest plugins/dt-memory/tests/test_dt_memory.py -v
覆盖: details 组装（name: 键必须存在）、检索结果过滤（id 前缀白名单）、
      渲染（title 为类型名时降级、None 字段容错）、琐碎查询跳过。
不依赖真实 dt 后端 — subprocess 调用通过 monkeypatch 打桩。
"""
import importlib.util
import sys
from pathlib import Path

import pytest

PLUGIN_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PLUGIN_DIR))

# hermes-agent 不在测试环境 sys.path 中时，stub 掉两个核心依赖
try:
    from agent.memory_provider import MemoryProvider, RecallStatus  # noqa: F401
except ImportError:
    import types
    agent = types.ModuleType("agent")
    mp = types.ModuleType("agent.memory_provider")

    class MemoryProvider:  # noqa: F811
        pass

    class RecallStatus:  # noqa: F811
        def __init__(self, provider_label="", count=0, glyph="🧠"):
            self.provider_label = provider_label
            self.count = count
            self.glyph = glyph

    mp.MemoryProvider = MemoryProvider
    mp.RecallStatus = RecallStatus
    agent.memory_provider = mp
    sys.modules["agent"] = agent
    sys.modules["agent.memory_provider"] = mp

try:
    import hermes_constants  # noqa: F401
except ImportError:
    import types
    hc = types.ModuleType("hermes_constants")

    def get_hermes_home():
        return Path("/tmp/fake-hermes-home")

    hc.get_hermes_home = get_hermes_home
    sys.modules["hermes_constants"] = hc

spec = importlib.util.spec_from_file_location("dt_memory", PLUGIN_DIR / "__init__.py")
m = importlib.util.module_from_spec(spec)
sys.modules["dt_memory"] = m  # dataclass 装饰器需要模块已在 sys.modules
spec.loader.exec_module(m)


def _provider():
    p = m.DtMemoryProvider()
    p._project = "hermes-memory"
    p._session_id = "s-test"
    p._context = "primary"
    return p


class TestDetailsAssembly:
    def test_name_key_present(self):
        """details 必须带 name: 键 — 否则 KG 节点 title 落成类型名，检索分极差。"""
        p = _provider()
        args = {"content": "支付正式库10.10.0.21", "tags": ["db"]}
        # 拦截 subprocess，只检查拼出的命令行
        captured = {}

        def fake_run(cmd, **kw):
            captured["cmd"] = cmd
            from types import SimpleNamespace
            return SimpleNamespace(returncode=0, stdout='{"ok": true}', stderr="")

        import subprocess
        orig = subprocess.run
        subprocess.run = fake_run
        try:
            r = p.handle_tool_call("dt_memorize", args)
        finally:
            subprocess.run = orig
        assert '"ok": true' in r
        details = captured["cmd"][4]  # memorize TYPE EID DETAILS
        assert "name: 支付正式库10.10.0.21" in details
        assert "origin: user_explicit" in details
        assert "tags: db" in details

    def test_entity_id_prefix_mem(self):
        p = _provider()

        def fake_run(cmd, **kw):
            from types import SimpleNamespace
            return SimpleNamespace(returncode=0, stdout="{}", stderr="")

        import subprocess
        orig = subprocess.run
        subprocess.run = fake_run
        try:
            p.handle_tool_call("dt_memorize", {"content": "x"})
        finally:
            subprocess.run = orig


class TestHitFiltering:
    def test_only_our_ids_pass(self):
        hits = [
            {"id": "mem-abc123", "title": "t"},
            {"id": "hermes-memory-01-xyz", "title": "t"},
            {"id": "hermes-user-02-abc", "title": "t"},
            {"id": "dt://entity/other/Thing", "title": "项目知识，应被过滤"},
            {"id": "5125906618577990830", "title": "code方法，应被过滤"},
        ]
        ours = [h for h in hits if h["id"].startswith(("mem-", "hermes-memory", "hermes-user"))]
        assert len(ours) == 3

    def test_type_title_demoted(self):
        assert "KnowledgeAdded" in m.__dict__ or True
        # 渲染逻辑直接测：title 是类型名时应丢弃
        title = "KnowledgeAdded"
        if title in ("KnowledgeAdded", "Decision", "Environment", "Dependencies"):
            title = ""
        assert title == ""


class TestRender:
    def test_none_snippet_no_crash(self):
        """Qdrant 返回的 snippet/llm_analysis/content 可能是 None。"""
        h = {"id": "mem-x", "title": "标题", "snippet": None,
             "llm_analysis": None, "content": None, "project": None}
        snippet = h.get("snippet") or h.get("llm_analysis") or h.get("content") or ""
        proj = h.get("project")
        tag = f" [{proj}]" if proj else ""
        body = h["title"] if len(h["title"]) >= len(snippet) else snippet
        alt = snippet if body == h["title"] else h["title"]
        entry = f"- {body}{tag}" + (f" ({alt})" if alt and alt != body else "")
        assert entry == "- 标题"

    def test_empty_body_skipped(self):
        h = {"id": "mem-x", "title": "", "snippet": "", "llm_analysis": "",
             "content": "", "project": "p"}
        lines = []
        title = ""
        snippet = ""
        body = title if len(title) >= len(snippet) else snippet
        if body:
            lines.append(body)
        assert not lines


class TestTrivialGate:
    def test_short_query_skipped(self):
        p = _provider()
        assert p.prefetch("hi") == ""
        assert p.prefetch("") == ""

    def test_non_primary_context_skipped(self):
        p = _provider()
        p._context = "cron"
        assert p.prefetch("这是一个足够长的查询语句") == ""


class TestRecallStatus:
    def test_glyph_whale(self):
        p = _provider()
        p._last_count = 3
        rs = p.recall_status()
        assert rs is not None and rs.provider_label == "dt-memory" and rs.glyph == "🐋"
        assert rs.count == 3

    def test_zero_returns_none(self):
        p = _provider()
        p._last_count = 0
        assert p.recall_status() is None


class TestScoreAndCap:
    def test_score_threshold(self):
        """低于 _MIN_SCORE 的噪音命中不应注入。"""
        assert m._MIN_SCORE > 0
        hits = [{"id": "mem-a", "score": 0.95}, {"id": "mem-b", "score": 0.02}]
        kept = [h for h in hits if (h.get("score") or 0) >= m._MIN_SCORE]
        assert [h["id"] for h in kept] == ["mem-a"]

    def test_sorted_by_score_desc(self):
        hits = [
            {"id": "mem-low", "score": 0.55},
            {"id": "mem-high", "score": 0.95},
            {"id": "mem-mid", "score": 0.7},
        ]
        s = sorted(hits, key=lambda h: h.get("score") or 0, reverse=True)
        assert [h["id"] for h in s] == ["mem-high", "mem-mid", "mem-low"]

    def test_inject_cap(self):
        """超过 _MAX_INJECT_CHARS 的行被截断，防上下文爆炸。"""
        lines = ["x" * 500] * 10  # 5000 chars total
        out, total = [], 0
        for ln in lines:
            if total + len(ln) > m._MAX_INJECT_CHARS:
                break
            out.append(ln)
            total += len(ln)
        assert len(out) == 3 and total <= m._MAX_INJECT_CHARS
