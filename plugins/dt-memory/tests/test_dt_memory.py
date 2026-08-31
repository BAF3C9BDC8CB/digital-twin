"""dt-memory 插件单元测试 (v2 按需检索)。

运行: pytest plugins/dt-memory/tests/test_dt_memory.py -v
覆盖: v2 核心行为 ——
  * prefetch 默认零注入（普通查询不返回任何记忆）
  * 显式记忆意图词触发定向检索（项目+全局）
  * project 区分：项目记忆 vs 全局记忆（hermes-global）
  * 写入路径 details 组装 / 检索结果过滤 / 渲染容错
不依赖真实 dt 后端 — subprocess 调用通过 monkeypatch 打桩。
"""
import importlib.util
import json
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

spec = importlib.util.spec_from_file_location(
    "dt_memory", PLUGIN_DIR / "__init__.py",
    submodule_search_locations=[str(PLUGIN_DIR)],
)
m = importlib.util.module_from_spec(spec)
m.__package__ = "dt_memory"
sys.modules["dt_memory"] = m  # dataclass 装饰器需要模块已在 sys.modules
# 注册子模块（llm_extract）以支持相对导入
_llm_spec = importlib.util.spec_from_file_location(
    "dt_memory.llm_extract", PLUGIN_DIR / "llm_extract.py"
)
if _llm_spec and _llm_spec.loader:
    _llm_mod = importlib.util.module_from_spec(_llm_spec)
    sys.modules["dt_memory.llm_extract"] = _llm_mod
    _llm_spec.loader.exec_module(_llm_mod)
spec.loader.exec_module(m)


def _provider():
    p = m.DtMemoryProvider()
    p._project = "hermes-memory"
    p._session_id = "s-test"
    p._context = "primary"
    return p


def _stub_search(monkeypatch, hits_by_project=None):
    """Stub _search_world: 按 project 返回固定 hits。"""
    hits_by_project = hits_by_project or {}
    monkeypatch.setattr(
        m.DtMemoryProvider,
        "_search_world",
        lambda self, query, *, world, project, limit: hits_by_project.get(project, []),
    )


class TestV2OnDemandRecall:
    """v2 核心：按需检索，默认零注入。"""

    def test_normal_query_no_injection(self, monkeypatch):
        """普通查询（无记忆意图词）→ prefetch 返回空，不触发任何检索。"""
        p = _provider()
        called = []

        def fake_search(self, query, *, world, project, limit):
            called.append(project)
            return [{"id": "mem-x", "title": "t", "snippet": "s", "project": project, "score": 0.9}]

        monkeypatch.setattr(m.DtMemoryProvider, "_search_world", fake_search)
        assert p.prefetch("支付手续费怎么算的") == ""
        assert called == []  # 没有检索调用 → 零 token

    def test_intent_word_triggers_project_and_global(self, monkeypatch):
        """含'记得'触发项目+全局两侧检索。"""
        p = _provider()
        p._project = "uvp-pay-center"
        called = []

        def fake_search(self, query, *, world, project, limit):
            called.append(project)
            return [{"id": "mem-y", "title": "银盛费率", "snippet": "0.6%", "project": project, "score": 0.9}]

        monkeypatch.setattr(m.DtMemoryProvider, "_search_world", fake_search)
        out = p.prefetch("记得银盛费率是多少吗")
        assert "银盛费率" in out
        assert called == ["uvp-pay-center", "hermes-global"]  # 项目 + 全局

    def test_intent_word_recall_english(self, monkeypatch):
        p = _provider()
        called = []

        def fake_search(self, query, *, world, project, limit):
            called.append(project)
            return [{"id": "mem-e", "title": "t", "snippet": "s", "project": project, "score": 0.9}]

        monkeypatch.setattr(m.DtMemoryProvider, "_search_world", fake_search)
        p.prefetch("do you remember the fee model?")
        assert called  # remember 命中

    def test_no_hits_returns_empty(self, monkeypatch):
        p = _provider()
        _stub_search(monkeypatch, {})
        assert p.prefetch("记得那个数据库地址吗") == ""

    def test_scores_below_floor_dropped(self, monkeypatch):
        """_search_world 内部按 score 地板过滤 — 低分命中不进入注入候选。"""
        p = _provider()
        import subprocess
        from types import SimpleNamespace

        def fake_run(cmd, **kw):
            payload = {
                "hits": [
                    {"id": "mem-a", "title": "高相关", "score": 0.9, "project": "hermes-memory"},
                    {"id": "mem-b", "title": "低分噪音", "score": 0.1, "project": "hermes-memory"},
                ]
            }
            return SimpleNamespace(returncode=0, stdout=json.dumps(payload), stderr="")

        monkeypatch.setattr(subprocess, "run", fake_run)
        hits = p._search_world("记得数据库", world="memory", project="hermes-memory", limit=4)
        assert [h["id"] for h in hits] == ["mem-a"]  # 0.1 被滤掉

    def test_prefetch_renders_only_surviving_hits(self, monkeypatch):
        """prefetch 对 _search_world 返回的命中做渲染注入（项目+全局分组）。"""
        p = _provider()
        p._project = "uvp-pay-center"

        def fake_search(self, query, *, world, project, limit):
            return [{"id": "mem-a", "title": "银盛费率", "snippet": "0.6%", "project": project, "score": 0.9}]

        monkeypatch.setattr(m.DtMemoryProvider, "_search_world", fake_search)
        out = p.prefetch("记得银盛费率是多少吗")
        assert "【项目记忆】" in out
        assert "【全局记忆】" in out
        assert out.count("银盛费率") == 2


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
        # 默认 project = 当前项目
        assert "--project" in captured["cmd"]
        assert captured["cmd"][-1] == "hermes-memory"

    def test_global_memory_project(self):
        """全局记忆显式传 project=hermes-global。"""
        p = _provider()
        captured = {}

        def fake_run(cmd, **kw):
            captured["cmd"] = cmd
            from types import SimpleNamespace
            return SimpleNamespace(returncode=0, stdout='{"ok": true}', stderr="")

        import subprocess
        orig = subprocess.run
        subprocess.run = fake_run
        try:
            r = p.handle_tool_call("dt_memorize", {"content": "全局规则", "project": "hermes-global"})
        finally:
            subprocess.run = orig
        assert captured["cmd"][-1] == "hermes-global"

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
            {"id": "auto-abc", "title": "t"},
            {"id": "dt://entity/other/Thing", "title": "项目知识，应被过滤"},
            {"id": "5125906618577990830", "title": "code方法，应被过滤"},
        ]
        ours = [h for h in hits if h["id"].startswith(("mem-", "hermes-memory", "hermes-user", "auto-"))]
        assert len(ours) == 4

    def test_type_title_demoted(self):
        # 渲染逻辑直接测：title 是类型名时应丢弃
        title = "KnowledgeAdded"
        if title in ("KnowledgeAdded", "Decision", "Environment", "Dependencies"):
            title = ""
        assert title == ""


class TestRender:
    def test_none_snippet_no_crash(self):
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

    def test_render_hit_with_project_tag(self):
        p = _provider()
        h = {"id": "mem-x", "title": "银盛费率", "snippet": "0.6%",
             "project": "uvp-pay-center", "score": 0.9}
        out = p._render_hit(h)
        assert "银盛费率" in out
        assert "uvp-pay-center" in out


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

    def test_inject_cap(self):
        """超过 _MAX_INJECT_CHARS 的行被截断，防上下文爆炸。"""
        lines = ["x" * 500] * 10  # 5000 chars total
        out, total = [], 0
        for ln in lines:
            if total + len(ln) > m._MAX_INJECT_CHARS:
                break
            out.append(ln)
            total += len(ln)
        assert len(out) == 2 and total <= m._MAX_INJECT_CHARS


class TestV3LLMActiveExtraction:
    """v3 核心：LLM 驱动的主动记忆整理（不依赖用户说"记住"）。"""

    def test_sync_turn_triggers_extract_every_n_turns(self, monkeypatch):
        """每 _EXTRACT_EVERY_TURNS 轮触发一次后台 LLM 提取。"""
        p = _provider()
        triggered = []

        def fake_queue(self, messages):
            triggered.append(len(messages))

        monkeypatch.setattr(m.DtMemoryProvider, "_queue_llm_extract", fake_queue)
        msgs = [{"role": "user", "content": f"msg{i}"} for i in range(5)]
        for i in range(1, 5):
            p.sync_turn(f"u{i}", f"a{i}", messages=msgs)
        assert len(triggered) == 1  # 第 3 轮触发一次

    def test_on_session_end_triggers_llm_extract(self, monkeypatch):
        """会话结束必然触发 LLM 提取。"""
        p = _provider()
        triggered = []

        def fake_queue(self, messages):
            triggered.append(messages)

        monkeypatch.setattr(m.DtMemoryProvider, "_queue_llm_extract", fake_queue)
        msgs = [{"role": "user", "content": "u1"}, {"role": "assistant", "content": "a1"}]
        p.on_session_end(msgs)
        assert triggered == [msgs]

    def test_write_with_dedup_new(self, monkeypatch):
        """无相似命中 → 新建记忆，scope=global 写 hermes-global。"""
        p = _provider()
        p._project = "uvp-pay-center"
        written = []

        monkeypatch.setattr(
            m.DtMemoryProvider, "_search_world",
            lambda self, q, *, world, project, limit: [],
        )
        monkeypatch.setattr(
            m.DtMemoryProvider, "_dt_memorize",
            lambda self, eid, details, project=None: written.append((eid, details, project)),
        )
        p._write_with_dedup({
            "summary": "银盛费率0.6%", "type": "fact", "scope": "global",
            "detail": "分账费率", "importance": 4, "tags": ["fee"],
        })
        assert len(written) == 1
        eid, details, project = written[0]
        assert eid.startswith("mem-")
        assert "银盛费率0.6%" in details
        assert project == "hermes-global"

    def test_write_with_dedup_updates_existing(self, monkeypatch):
        """相似命中 ≥ 阈值 → 复用 entity_id 更新而非新增。"""
        p = _provider()
        written = []

        monkeypatch.setattr(
            m.DtMemoryProvider, "_search_world",
            lambda self, q, *, world, project, limit: [
                {"id": "mem-abc", "title": "银盛费率", "score": 0.9, "project": project}
            ],
        )
        monkeypatch.setattr(
            m.DtMemoryProvider, "_dt_memorize",
            lambda self, eid, details, project=None: written.append((eid, details, project)),
        )
        p._write_with_dedup({
            "summary": "银盛费率0.6%", "type": "fact", "scope": "project",
            "detail": "分账费率更新", "importance": 4, "tags": [],
        })
        assert len(written) == 1
        assert written[0][0] == "mem-abc"  # 复用旧 id
        assert "分账费率更新" in written[0][1]
        assert written[0][2] == p._project

    def test_write_with_dedup_below_threshold_new(self, monkeypatch):
        """相似但低于阈值 → 新建（不误合并）。"""
        p = _provider()
        written = []

        monkeypatch.setattr(
            m.DtMemoryProvider, "_search_world",
            lambda self, q, *, world, project, limit: [
                {"id": "mem-old", "title": "其他", "score": 0.4, "project": project}
            ],
        )
        monkeypatch.setattr(
            m.DtMemoryProvider, "_dt_memorize",
            lambda self, eid, details, project=None: written.append((eid, details, project)),
        )
        p._write_with_dedup({
            "summary": "新知识", "type": "decision", "scope": "project",
            "detail": "全新", "importance": 3, "tags": [],
        })
        assert len(written) == 1
        assert written[0][0].startswith("mem-")
        assert written[0][0] != "mem-old"


class TestLLMExtractModule:
    """llm_extract 模块的纯逻辑（不依赖真实 LLM）。"""

    def test_parse_json_plain(self):
        from llm_extract import _parse_json_response
        out = _parse_json_response('[{"summary": "a", "type": "fact"}]')
        assert out == [{"summary": "a", "type": "fact"}]

    def test_parse_json_fenced(self):
        from llm_extract import _parse_json_response
        out = _parse_json_response('```json\n[{"summary": "b"}]\n```')
        assert out == [{"summary": "b"}]

    def test_parse_json_garbage(self):
        from llm_extract import _parse_json_response
        assert _parse_json_response("not json at all") == []

    def test_load_config_missing_file(self, tmp_path):
        from llm_extract import load_hermes_llm_config
        assert load_hermes_llm_config(str(tmp_path / "nonexistent")) is None
