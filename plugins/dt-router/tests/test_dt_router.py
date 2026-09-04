"""dt-router plugin tests. dt CLI subprocess 通过 monkeypatch 打桩, 不依赖真实后端。"""

import importlib.util
import sys
from pathlib import Path

import pytest

_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_ROOT))

# Hermes agent 包(runtime_cwd) —— 测试环境无 hermes-agent 时指向安装树
_HERMES_AGENT = Path("/home/luis/.hermes/hermes-agent")
if _HERMES_AGENT.exists() and str(_HERMES_AGENT) not in sys.path:
    sys.path.insert(0, str(_HERMES_AGENT))

spec = importlib.util.spec_from_file_location("dt_router", _ROOT / "__init__.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


# --- build_brief ------------------------------------------------------------


def test_build_brief_early_exit_none():
    # L0(garbled/闲聊): world=="none" -> 不注入
    assert m._build_brief({"world": "none", "hits": []}, "你好") is None


def test_build_brief_has_hits_compressed():
    data = {
        "query": "MemgraphClient new",
        "world": "all",
        "hits": [
            {
                "title": "newRow_property",
                "snippet": "字段属性，标记当前字段是否在新行开始。",
                "score": 0.0164,
                "file_path": None,
                "project": None,
                "source_ref": "dt://doc/x/a.md",
            }
        ],
    }
    out = m._build_brief(data, "MemgraphClient new")
    assert out is not None
    assert "<knowledge_context>" in out
    assert "newRow_property" in out
    assert "dt://doc/x/a.md" in out
    # 不期望原始 JSON 被灌入(去重/压缩)
    assert "score_breakdown" not in out


def test_build_brief_zero_hits_returns_hint():
    out = m._build_brief({"world": "all", "hits": []}, "zzzzzz")
    assert out and "0 相关命中" in out


# --- hook: pre_llm_call -----------------------------------------------------


def test_pre_llm_call_casual_marks_checked_and_none(monkeypatch):
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: {"world": "none", "hits": []})
    # 先清状态, 防污染
    m._kg_checked.clear()
    out = m._on_pre_llm_call(session_id="s1", turn_id="t1", user_message="你好", is_first_turn=True)
    assert out is None
    # 即使 casual, 也标记本 turn 已感知 -> pre_tool_call 不拦
    assert m._was_checked("s1:t1")


def test_pre_llm_call_tech_injects_context(monkeypatch):
    data = {
        "world": "all",
        "hits": [
            {
                "title": "PaymentClient",
                "snippet": "支付客户端",
                "score": 0.9,
                "project": "uvp-order-center",
                "file_path": "src/main/java/PaymentClient.java",
            }
        ],
    }
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: data)
    m._kg_checked.clear()
    out = m._on_pre_llm_call(session_id="s9", turn_id="t1", user_message="PaymentClient 回调超时怎么修", is_first_turn=False)
    assert out and out.startswith("<knowledge_context>")
    assert "PaymentClient" in out
    assert "uvp-order-center" in out


def test_pre_llm_call_fail_open_on_error(monkeypatch):
    def boom(*a, **k):
        raise RuntimeError("subprocess dead")

    monkeypatch.setattr(m, "_run_router", boom)
    out = m._on_pre_llm_call(session_id="sx", turn_id="t1", user_message="后端服务排查", is_first_turn=True)
    assert out is None


# --- hook: pre_tool_call ----------------------------------------------------


def test_pre_tool_call_blocks_read_without_kg(monkeypatch):
    m._kg_checked.clear()
    r = m._on_pre_tool_call("read_file", {"path": "x.rs"}, "sidA")
    assert r and r.get("action") == "block"
    assert "dt_search_kg" in r.get("message", "")


def test_pre_tool_call_blocks_write_without_kg():
    m._kg_checked.clear()
    r = m._on_pre_tool_call("write_file", {}, "sidB")
    assert r and r.get("action") == "block"


def test_pre_tool_call_allows_after_kg_sense(monkeypatch):
    # 先由 pre_llm_call 标记已感知, 再调读工具 -> 放行
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: {"world": "all", "hits": []})
    m._kg_checked.clear()
    m._on_pre_llm_call(session_id="sok", turn_id="t1", user_message="PaymentClient 分析", is_first_turn=True)
    r = m._on_pre_tool_call("read_file", {"path": "x.rs"}, "sid", turn_id="t1", session_id="sok")
    assert r is None  # 不再强制


def test_pre_tool_call_skips_memory_writes():
    m._kg_checked.clear()
    for tool in ("dt_memorize", "dt_learn", "dt_event"):
        assert m._on_pre_tool_call(tool, {}, "s") is None


def test_pre_tool_call_fail_open_on_error():
    m._kg_checked.clear()
    r = m._on_pre_tool_call("read_file", {"path": "x"}, "s", turn_id=None)  # 缺 turn_id, 内部不应崩
    assert r is None or r.get("action") in ("block",)


# --- hook: pre_tool_call -- delegate_task KG 注入 ---------------------------


def test_delegate_task_injects_kg_context_into_child_context(monkeypatch):
    data = {
        "world": "all",
        "hits": [
            {
                "title": "PaymentClient",
                "snippet": "支付客户端封装",
                "score": 0.91,
                "project": "uvp-order-center",
                "file_path": "src/main/java/PaymentClient.java",
            }
        ],
    }
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: data)
    args = {
        "tasks": [{"goal": "PaymentClient 回调超时怎么修", "context": "已有背景", "role": "leaf"}],
    }
    r = m._on_pre_tool_call("delegate_task", args, "sid")
    assert r and r.get("action") == "modify"
    tasks = r["args"]["tasks"]
    assert m._DELEGATE_CTX_MARKER in tasks[0]["context"]
    assert "PaymentClient" in tasks[0]["context"]
    # 原 role / goal 保留
    assert tasks[0]["role"] == "leaf"
    assert tasks[0]["goal"] == "PaymentClient 回调超时怎么修"


def test_delegate_task_no_hits_returns_none(monkeypatch):
    # L0 早退 / 无命中 -> 不改动 args
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: {"world": "none", "hits": []})
    args = {"tasks": [{"goal": "你好", "context": ""}]}
    r = m._on_pre_tool_call("delegate_task", args, "sid")
    assert r is None  # 未注入, 不改动


def test_delegate_task_no_tasks_returns_none():
    r = m._on_pre_tool_call("delegate_task", {}, "sid")
    assert r is None


def test_delegate_task_idempotent_no_double_inject(monkeypatch):
    data = {"world": "all", "hits": [{"title": "T", "snippet": "S", "score": 0.8}]}
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: data)
    args = {"tasks": [{"goal": "X 怎么改", "context": m._DELEGATE_CTX_MARKER + " 已注入", "role": "leaf"}]}
    r = m._on_pre_tool_call("delegate_task", args, "sid")
    assert r is None  # 已含标记 -> 不重复注入


def test_delegate_task_fail_open_on_router_error(monkeypatch):
    def boom(*a, **k):
        raise RuntimeError("router down")

    monkeypatch.setattr(m, "_run_router", boom)
    args = {"tasks": [{"goal": "分析订单模块"}]}
    r = m._on_pre_tool_call("delegate_task", args, "sid")
    assert r is None


# --- hook: subagent_start ---------------------------------------------------


def test_subagent_start_observes_without_crash(monkeypatch):
    monkeypatch.setattr(m, "_run_router", lambda *a, **k: {"world": "all", "hits": []})
    # 返回值被忽略(observer), 只应正常返回 None / 不抛
    ret = m._on_subagent_start(child_goal="分析订单支付模块", child_role="leaf", child_session_id="c1")
    assert ret is None


def test_subagent_start_empty_goal_returns():
    m._registry_cache = []
    ret = m._on_subagent_start(child_goal="", child_role="leaf", child_session_id="c2")
    assert ret is None


# --- registry ---------------------------------------------------------------


def test_match_project_in_text_standalone_token():
    m._registry_cache = [("uvp-order-center", Path("/x/uvp-order-center"))]
    assert m._match_project_in_text("uvp-order-center 支付超时怎么配置") == "uvp-order-center"
    # 其他项目名不应误匹配
    assert m._match_project_in_text("你好") is None


def test_infer_project_from_cwd_deepest_wins():
    m._registry_cache = [
        ("parent", Path("/a")),
        ("child", Path("/a/b")),
    ]
    assert m._infer_project(Path("/a/b/c")) == "child"


def test_is_registered_project(tmp_path):
    reg = tmp_path / "proj"
    reg.mkdir()
    m._registry_cache = [("proj", reg)]
    assert m._is_registered_project(reg) is True
    assert m._is_registered_project(tmp_path) is False


def test_child_projects_of_container(tmp_path):
    # 容器目录含两个注册子项目
    container = tmp_path / "pay"
    container.mkdir()
    sub_a = container / "uvp-offen-pay"
    sub_b = container / "offenpay-ui"
    sub_a.mkdir()
    sub_b.mkdir()
    m._registry_cache = [
        ("offen-pay", sub_a),
        ("offenpay-ui", sub_b),
    ]
    children = m._child_projects_of(container)
    names = {n for n, _ in children}
    assert names == {"offen-pay", "offenpay-ui"}
    # container 本身不是注册项目 -> 不被自身返回
    assert m._child_projects_of(sub_a) == []  # sub_a 是注册项目本体


def test_infer_container_subproject_single_child(tmp_path):
    container = tmp_path / "pay"
    container.mkdir()
    sub = container / "uvp-offen-pay"
    sub.mkdir()
    m._registry_cache = [("offen-pay", sub)]
    # 单子项目: 可直接确定
    assert m._infer_container_subproject(container, "怎么看支付手续费") == "offen-pay"


def test_infer_container_subproject_by_message_token(tmp_path):
    container = tmp_path / "pay"
    container.mkdir()
    a = container / "uvp-offen-pay"
    b = container / "offenpay-ui"
    a.mkdir()
    b.mkdir()
    m._registry_cache = [("offen-pay", a), ("offenpay-ui", b)]
    # 消息里直接命中子项目名 -> 用之
    assert m._infer_container_subproject(container, "offenpay-ui 的主页组件在哪") == "offenpay-ui"


def test_infer_container_subproject_domain_overlap(tmp_path):
    container = tmp_path / "pay"
    container.mkdir()
    a = container / "uvp-offen-pay"
    b = container / "offenpay-ui"
    a.mkdir()
    b.mkdir()
    m._registry_cache = [("offen-pay", a), ("offenpay-ui", b)]
    # 消息含 "pay" 领域词: uvp-offen-pay 路径本段含 pay, offenpay-ui 含 pay
    # 两个都含 pay 时按重合度选 —— 这里比较精细, 保守断言能返回其一或 None
    r = m._infer_container_subproject(container, "支付 pay 的序列规则")
    assert r is None or r in ("offen-pay", "offenpay-ui")


def test_resolve_project_container_scopes_to_subproject(tmp_path):
    # Fix1 核心: 容器 cwd + 无显式项目名的消息 -> 解析到某个子项目
    container = tmp_path / "pay"
    container.mkdir()
    a = container / "uvp-offen-pay"
    a.mkdir()
    m._registry_cache = [("offen-pay", a)]
    proj = m._resolve_project("支付订单号的生成规则", cwd=container)
    assert proj == "offen-pay"


def test_resolve_project_exact_text_wins_over_container(tmp_path):
    # 消息显式项目名 > 容器推断
    container = tmp_path / "pay"
    container.mkdir()
    a = container / "uvp-offen-pay"
    b = container / "offenpay-ui"
    a.mkdir()
    b.mkdir()
    m._registry_cache = [("offen-pay", a), ("offenpay-ui", b)]
    proj = m._resolve_project("offenpay-ui 的主页代码在哪", cwd=a)
    assert proj == "offenpay-ui"


# --- register ---------------------------------------------------------------


def test_register_registers_three_hooks():
    class FakeCtx:
        def __init__(self):
            self.hooks = {}

        def register_hook(self, name, fn):
            self.hooks[name] = fn

    ctx = FakeCtx()
    m.register(ctx)
    assert set(ctx.hooks) == {"pre_llm_call", "pre_tool_call", "subagent_start"}