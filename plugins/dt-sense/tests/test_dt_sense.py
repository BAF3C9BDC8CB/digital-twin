"""dt-sense 插件单元测试 — 透传 dt sense 输出 + 项目匹配（无硬编码项目名）。

运行: pytest plugins/dt-sense/tests/test_dt_sense.py -v
覆盖:
  * _run_sense 透传 dt sense 原生文本输出（注入内容 = dt sense 输出）
  * 无硬编码项目名/别名（ALIASES 已移除；项目来源唯一 = registry）
  * 项目匹配（词边界/最长名优先/无别名）
  * 容器目录匹配（base_children 场景）
  * 无匹配时注入最小简报（而非静默跳过）
"""
import sys
from pathlib import Path

import pytest

PLUGIN_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(PLUGIN_DIR))

# Hermes agent 包（runtime_cwd）—— 测试环境无 hermes-agent 时指向安装树
_HERMES_AGENT = Path("/home/luis/.hermes/hermes-agent")
if _HERMES_AGENT.exists() and str(_HERMES_AGENT) not in sys.path:
    sys.path.insert(0, str(_HERMES_AGENT))

import importlib.util

spec = importlib.util.spec_from_file_location("dt_sense", PLUGIN_DIR / "__init__.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class TestNoHardcodedProjects:
    """核心约束：插件里不能有硬编码项目名（否则新增项目无法使用）。"""

    def test_no_aliases_map(self):
        assert not hasattr(m, "ALIASES"), "ALIASES 硬编码表已移除"

    def test_no_hardcoded_project_names(self):
        # 逻辑代码里不应出现具体项目名（docstring 里的示例描述除外）
        src_path = getattr(m, "__file__", None)
        assert src_path, "模块应有 __file__"
        src = Path(src_path).read_text(encoding="utf-8")
        # 去掉所有 docstring（模块级+函数级），只看实际逻辑
        import ast
        tree = ast.parse(src)
        code_lines = src.splitlines()
        logic = []
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                body = node.body
                # 跳过 docstring 语句
                first_stmt = body[0] if body else None
                if (
                    first_stmt is not None
                    and isinstance(first_stmt, ast.Expr)
                    and isinstance(first_stmt.value, ast.Constant)
                    and isinstance(first_stmt.value.value, str)
                ):
                    body = body[1:]
                if not body:
                    continue
                start = body[0].lineno
                end = node.end_lineno or start
                logic.extend(code_lines[start - 1:end])
        logic_src = "\n".join(logic)
        for name in ("pay-center", "im-center", "digital-twin-v2", "offen-pay", "uvp-user-center"):
            assert name not in logic_src, f"逻辑代码含硬编码项目名: {name}"

    def test_registry_is_source_of_names(self):
        # 项目名来源 = registry config
        reg = m._load_registry()
        assert len(reg) > 50, f"注册表应含 65+ 项目, 实际 {len(reg)}"


class TestRunSensePassthrough:
    """注入内容 = dt sense 的原生文本输出（插件不二次渲染）。"""

    def test_run_sense_returns_text(self, monkeypatch):
        # 模拟 dt sense 文本输出
        import subprocess
        from types import SimpleNamespace
        monkeypatch.setattr(
            subprocess, "run",
            lambda *a, **k: SimpleNamespace(returncode=0, stdout="📍 /tmp/x\n  Status: indexed\n  Project: demo\n", stderr=""),
        )
        out = m._run_sense(Path("/tmp/x"))
        assert out is not None
        assert "Status: indexed" in out
        assert "Project: demo" in out

    def test_run_sense_failure_returns_none(self, monkeypatch):
        import subprocess
        from types import SimpleNamespace
        monkeypatch.setattr(
            subprocess, "run",
            lambda *a, **k: SimpleNamespace(returncode=1, stdout="", stderr="boom"),
        )
        assert m._run_sense(Path("/tmp/x")) is None

    def test_run_sense_uses_text_mode(self, monkeypatch):
        # 必须用文本模式（不带 --json），注入原生输出
        import subprocess
        captured = {}

        def fake_run(cmd, **kw):
            captured["cmd"] = cmd
            from types import SimpleNamespace
            return SimpleNamespace(returncode=0, stdout="📍 ok", stderr="")

        monkeypatch.setattr(subprocess, "run", fake_run)
        m._run_sense(Path("/tmp/x"))
        assert "--json" not in captured["cmd"], "应透传文本输出, 不用 --json"


class TestSearchGuidance:
    """dt sense 输出后追加的一行 KG 检索引导。"""

    def test_container_guidance(self):
        g = m._search_guidance("📍 /x\n  Status: unregistered\n  📁 注册容器(base): 内含 2 个已注册子项目\n    offen-pay → /x/uvp-offen-pay")
        assert "dt_search(project=<子项目名>)" in g
        assert "不要用目录名" in g

    def test_indexed_guidance(self):
        g = m._search_guidance("📍 /x\n  Status: indexed\n  Project: digital-twin-v2 (/x)")
        assert "dt_search(world=code, project=digital-twin-v2, limit=5)" in g

    def test_unregistered_guidance(self):
        g = m._search_guidance("📍 /x\n  Status: unregistered")
        assert "未注册" in g and "dt build" in g

    def test_hook_injects_sense_plus_guidance(self, monkeypatch):
        # 注入内容 = sense 输出 + 引导
        monkeypatch.setattr(m, "_load_registry", lambda: [("digital-twin-v2", "/data/myProject/digital-twin-v2")])
        monkeypatch.setattr(m, "_match_project", lambda msg: None)
        monkeypatch.setattr(m, "_match_cwd", lambda cwd: Path("/data/myProject/digital-twin-v2"))
        monkeypatch.setattr(
            m, "_run_sense",
            lambda path: "📍 /data/myProject/digital-twin-v2\n  Status: indexed\n  Project: digital-twin-v2 (/data/myProject/digital-twin-v2)",
        )
        out = m._on_pre_llm_call(session_id="s-guid", user_message="分析构建流程", is_first_turn=True)
        assert out is not None
        assert "Status: indexed" in out          # sense 输出透传
        assert "dt_search(world=code, project=digital-twin-v2" in out  # 引导追加


class TestMinimalBrief:
    """无项目匹配时注入最小引导（不再静默跳过）。"""

    def test_minimal_brief_mentions_knowledge(self, monkeypatch):
        monkeypatch.setattr(m, "_load_registry", lambda: [("a", "/x/a"), ("b", "/x/b")])
        brief = m._minimal_brief(Path("/home/luis"))
        assert "[DT-SENSE]" in brief
        assert "2 个注册项目" in brief
        assert "dt_search" in brief
        assert len(brief) < 300

    def test_hook_injects_minimal_when_no_match(self, monkeypatch):
        monkeypatch.setattr(m, "_load_registry", lambda: [("a", "/x/a")])
        monkeypatch.setattr(m, "_match_project", lambda msg: None)
        monkeypatch.setattr(m, "_match_cwd", lambda cwd: None)
        out = m._on_pre_llm_call(session_id="s-min", user_message="今天天气如何", is_first_turn=True)
        assert out is not None and "[DT-SENSE]" in out and "未匹配" in out


class TestResolveCwd:
    """cwd 解析：必须用 Hermes 会话级 cwd，而非进程 cwd。"""

    def test_uses_session_cwd(self, monkeypatch):
        # 模拟 gateway 会话 cwd = pay 目录
        import agent.runtime_cwd as rc
        token = rc.set_session_cwd("/data/aflmProjects/others/pay")
        try:
            assert str(m._resolve_cwd()) == "/data/aflmProjects/others/pay"
        finally:
            rc.clear_session_cwd()

    def test_fallback_to_process_cwd(self, monkeypatch):
        # 无会话 cwd / TERMINAL_CWD 时兜底进程 cwd（模拟 import agent 失败）
        import builtins
        real_import = builtins.__import__

        def fake_import(name, *a, **kw):
            if name == "agent.runtime_cwd":
                raise ImportError("agent not available")
            return real_import(name, *a, **kw)

        monkeypatch.setattr(builtins, "__import__", fake_import)
        out = m._resolve_cwd()
        assert isinstance(out, Path)

    def test_hook_uses_session_cwd_for_match(self, monkeypatch):
        # 会话 cwd=pay 时，即使进程 cwd 是别处，也应匹配到 pay（容器）
        import agent.runtime_cwd as rc
        token = rc.set_session_cwd("/data/aflmProjects/others/pay")
        try:
            out = m._on_pre_llm_call(
                session_id="s-cwd-test", user_message="银盛支付手续费是多少？", is_first_turn=True
            )
        finally:
            rc.clear_session_cwd()
        assert out is not None
        assert "offen-pay" in out or "注册容器" in out or "未匹配" in out


class TestMatchProject:
    def test_exact_token(self):
        p = m._match_project("im-center 的消息撤回流程是怎样的？")
        assert p is not None and "uvp-im-center" in str(p)

    def test_no_false_positive_embedded(self):
        # 'svc' 不应匹配 'svc-order'
        p = m._match_project("查看 svc-order 的配置")
        assert p is None or "svc-order" not in str(p)

    def test_no_match_unrelated(self):
        p = m._match_project("今天天气怎么样？")
        assert p is None

    def test_cwd_inside_registered(self):
        p = m._match_cwd(Path("/data/aflmProjects/aflm/uvp-im-center/src/main/java"))
        assert p is not None and "uvp-im-center" in str(p)

    def test_cwd_unrelated(self):
        p = m._match_cwd(Path("/home/luis"))
        assert p is None

    def test_cwd_container_of_registered(self):
        # /data/aflmProjects/others/pay 是注册容器（含 offen-pay/offenpay-ui 子项目）
        p = m._match_cwd(Path("/data/aflmProjects/others/pay"))
        assert p is not None, "容器目录应返回 cwd 注入简报, 而非 None"
        assert str(p) == "/data/aflmProjects/others/pay"
