#!/usr/bin/env python3
"""每日会话知识图谱(Knowledge Graph)行为审计。

目的 (来自用户需求):
  每天 01:00 审计前一天的所有 AI 会话，核查它们是否遵守了 AGENTS.md 的 KG 行为准则：
    1. 是否查询了 KG？ (服务/配置/部署/历史决策场景应先查 world=memory)
    2. 查询时机是否正确？ (应在深度读代码之前先 dt_sense / dt_search_kg)
    3. 查询结果是否真正被使用？ (查完是否引用返回内容，而非查完即弃)
    4. 是否重复搜索代码？ (同一 read_file/search_files 目标单会话内多次)
    5. 搜索代码是否用到了 KG 已构建的代码？ (定位代码应先 dt_search_kg(world=code))
    6. 是否应该 memorize 却没 memorized？ (用户说"记住/记忆"但未调 dt_memorize)
    7. 是否遗漏历史决策？ (涉及历史决策但未先查 world=memory)

数据源:
  ~/.hermes/state.db 的 messages 表 (逐条消息 + tool_calls JSON)，覆盖全平台全会话。
  用 request_dump 不完整故弃用; state.db 才是完整行为真相源。

实现深度 (方案乙: 脚本提取证据 + LLM 语义复审):
  - 本脚本做确定性证据提取与指标计算 (客观、可追溯、零 token)。
  - 产出 structured JSON 证据 + 人类可读 Markdown 报告。
  - 报告的"语义裁决"部分 (查询结果是否被用/是否该记) 标记启发式结论，
    供上层 LLM 复审环节人工/LLM 确认。

用法:
  python3 audit_daily_sessions.py [--date YYYY-MM-DD] [--db ~/.hermes/state.db]
                                  [--json] [--report-out /path/to/report.md]
  AUDIT_DATE 环境变量亦可指定目标日期。
"""

import argparse
import datetime as dt
import json
import os
import sqlite3
import sys
from collections import Counter, defaultdict
from pathlib import Path

# ── 常量与配置 ──────────────────────────────────────────────────────
DEFAULT_DB = str(Path.home() / ".hermes" / "state.db")
REPEAT_SEARCH_THRESHOLD = int(os.environ.get("AUDIT_REPEAT_THRESHOLD", "2"))  # 判定项 D, N=2

# 工具名分类 (匹配 assistant tool_calls 里 function.name 或 tool_call arguments.name)
KG_SEARCH_TOOLS = {
    "dt_search_kg", "mcp__dt_mcp__dt_search_kg",
    "dt_search", "mcp__dt_mcp__dt_search",
}
KG_SENSE_TOOLS = {
    "dt_sense", "mcp__dt_mcp__dt_sense",
}
KG_MEMORIZE_TOOLS = {
    "dt_memorize", "mcp__dt_mcp__dt_memorize",
    "dt_memorize_kg", "mcp__dt_mcp__dt_memorize_kg",
    "dt_learn", "mcp__dt_mcp__dt_learn",
}
KG_RAW_QUERY_TOOLS = {
    "run_cypher_query", "mcp__memgraph__run_cypher_query",
    "search_kg", "mcp__dt_mcp__dt_kg_search",
}

# 代码定位工具 (读源码前应先用 KG 定位; 这些是"实际读代码"的信号)
CODE_READ_TOOLS = {
    "read_file", "search_files", "search_file", "grep", "rg",
}
# 深度探索工具 (在何时出现决定"查询时机")
DEEP_EXPLORE_TOOLS = {
    "read_file", "search_files", "patch", "write_file",
    "search_content", "search_file",
}

# 触发用户"记忆"意图的关键词 (出现在 user 消息中 → 应立即 dt_memorize)
MEMORIZE_INTENT_KEYWORDS = [
    "记住", "记一下", "记住这个", "记下来", "记忆",
    "请你记住", "请记住", "把这个记下来",
]
# 涉及历史决策/前情回顾的关键词 (应先查 world=memory)
HISTORY_DECISION_KEYWORDS = [
    "之前", "上次", "历史", "当时", "以前", "之前说过", "此前",
    "之前决定", "上次讨论", "历史决策", "之前配置", "之前部署",
]


class AuditSession:
    """单个会话在审计窗口内的行为聚合。"""

    def __init__(self, session_id: str):
        self.session_id = session_id
        self.msg_count = 0
        self.events: list[dict] = []  # 有序工具事件
        self.user_texts: list[str] = []
        self.assistant_texts: list[str] = []
        self.start_ts: float | None = None
        self.end_ts: float | None = None
        # 各类信号
        self.kg_searches: int = 0            # dt_search_kg 次数
        self.kg_sense: int = 0               # dt_sense 次数
        self.kg_memorize: int = 0            # dt_memorize/dt_learn 次数
        self.kg_raw_query: int = 0           # run_cypher_query 次数
        self.code_reads: int = 0             # read_file/search_files 总数
        self.kgs_runs: list[dict] = []       # 每次 KG 查询 (次数)
        self.kg_contexts: list[dict] = []    # 每次 KG 查询的前后上下文 (供 LLM 裁决"结果是否被用")
        self.repeated_code_searches: list[dict] = []  # 疑似重复
        self.memorize_intent_hits: list[dict] = []    # 用户说记住但未记
        self.history_intent_hits: list[dict] = []     # 涉及历史但未先查 KG

    def add_event(self, ts: float, tool_name: str, args: str | None, sig: str | None):
        """按顺序记录一次工具调用。sig 用于判重 (read_file 的 path / search 的 pattern)。"""
        if self.start_ts is None:
            self.start_ts = ts
        self.end_ts = ts
        self.events.append({
            "ts": ts,
            "tool": tool_name,
            "args": (args or "")[:200],
            "sig": sig or tool_name,
        })
        if tool_name in KG_SEARCH_TOOLS:
            self.kg_searches += 1
            self.kgs_runs.append({"ts": ts, "tool": tool_name})
        elif tool_name in KG_SENSE_TOOLS:
            self.kg_sense += 1
        elif tool_name in KG_MEMORIZE_TOOLS:
            self.kg_memorize += 1
        elif tool_name in KG_RAW_QUERY_TOOLS:
            self.kg_raw_query += 1
        elif tool_name in CODE_READ_TOOLS:
            self.code_reads += 1

    def capture_kg_context(self, ts: float, tool: str, args: str | None,
                           prev_user: str | None, next_assistant: str | None):
        """记录一次 KG 查询的前后上下文片段，供 LLM 语义裁决结果是否被真正使用。"""
        self.kg_contexts.append({
            "ts": ts,
            "tool": tool,
            "args": (args or "")[:500],
            "prev_user": (prev_user or "")[:300],
            "next_assistant": (next_assistant or "")[:600],
        })


def parsetime(ts: float) -> str:
    try:
        return dt.datetime.fromtimestamp(ts).isoformat(timespec="seconds")
    except Exception:
        return str(ts)


def extract_sig_from_args(tool: str, args: str) -> str | None:
    """从工具参数中提取用于判重的签名 (目标文件/路径)。

    判重基准是"被操作的目标文件"，而非搜索 pattern：
    - read_file / search_files / search_file: 用 path 参数 (目标文件/目录)
    - terminal: 粗提文件名
    因为同一文件用不同 pattern 反复搜索属正常探索，真正要抓的是
    "对同一文件反复读取却未借助 dt_search_kg(world=code) 定位"。
    """
    if not args:
        return None
    try:
        a = json.loads(args) if args.strip().startswith("{") else {}
    except Exception:
        a = {}
    # read_file / search_files / search_file: 优先用 path (目标文件/目录)
    if tool in ("read_file", "search_files", "search_file", "read_file_content"):
        for k in ("path", "file_path", "file"):
            if k in a and a[k]:
                return f"{tool}:{a[k]}"
        # 兜底: 其他键
        for k in ("pattern", "name"):
            if k in a and a[k]:
                return f"{tool}:{a[k]}"
        return f"{tool}:{args[:60]}"
    # terminal: 粗提文件名
    if tool == "terminal":
        txt = str(a.get("command", args))
        import re
        m = re.findall(r"[\w./\-]+\.(?:rs|py|ts|tsx|js|go|java|yaml|yml|json|toml|sh|md)\b", txt)
        return (f"{tool}:{m[0]}" if m else f"{tool}:{txt[:40]}")
    return f"{tool}:{args[:60]}"


def load_deviation_events(db: str, date: str):
    """加载某日期 (本地时区) 所有会话的 assistant 工具事件 + user 文本。"""
    conn = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    cur = conn.cursor()
    # 先取该日期的所有消息 (含 user/assistant/tool)，按时间排序
    cur.execute("""
        SELECT session_id, role, content, tool_calls, tool_name, timestamp
        FROM messages
        WHERE date(timestamp, 'unixepoch', 'localtime') = ?
        ORDER BY timestamp ASC
    """, (date,))
    rows = cur.fetchall()
    conn.close()

    sessions: dict[str, AuditSession] = {}
    pending_kg: dict[str, dict] = {}  # session_id -> 待补 next_assistant 的 KG 上下文
    for r in rows:
        sid = r["session_id"]
        if sid not in sessions:
            sessions[sid] = AuditSession(sid)
        s = sessions[sid]
        s.msg_count += 1
        ts = r["timestamp"]
        role = r["role"]

        if role == "user":
            txt = r["content"] or ""
            s.user_texts.append(txt)
            # 记忆意图检测
            for kw in MEMORIZE_INTENT_KEYWORDS:
                if kw in txt:
                    s.memorize_intent_hits.append({"ts": ts, "kw": kw, "text": txt[:120]})
                    break
            # 历史决策意图检测
            for kw in HISTORY_DECISION_KEYWORDS:
                if kw in txt:
                    s.history_intent_hits.append({"ts": ts, "kw": kw, "text": txt[:120]})
                    break
        elif role == "assistant":
            if r["content"]:
                s.assistant_texts.append(r["content"])
                # 若是 KG 查询后的第一条 assistant 文本，补 next_assistant
                if sid in pending_kg and pending_kg[sid].get("waiting_assistant"):
                    pending_kg[sid]["next_assistant"] = r["content"]
                    pending_kg[sid]["waiting_assistant"] = False
                    # 挪入 session 的 kg_contexts
                    s.kg_contexts.append(pending_kg[sid])
                    del pending_kg[sid]
            tc = r["tool_calls"]
            if tc and tc != "[]":
                try:
                    for t in json.loads(tc):
                        fn = (t.get("function") or {})
                        name = fn.get("name") or t.get("name")
                        args = fn.get("arguments") or json.dumps(t.get("arguments", {}))
                        if not name:
                            continue
                        # tool_call 包装的 MCP 工具名在 arguments.name
                        if name == "tool_call":
                            try:
                                inner = json.loads(args) if isinstance(args, str) else args
                                name = inner.get("name", name)
                                args = json.dumps(inner.get("arguments", {}))
                            except Exception:
                                pass
                        sig = extract_sig_from_args(name, args)
                        s.add_event(ts, name, args, sig)
                        # 捕获 KG 查询上下文: 触发"等待 assistant 回复"
                        if name in KG_SEARCH_TOOLS or name in KG_SENSE_TOOLS or name in KG_RAW_QUERY_TOOLS:
                            prev_user = s.user_texts[-1] if s.user_texts else ""
                            pending_kg[sid] = {
                                "ts": ts,
                                "tool": name,
                                "args": (args or "")[:500],
                                "prev_user": prev_user[:300],
                                "next_assistant": None,
                                "waiting_assistant": True,
                            }
                except Exception:
                    pass

    # 会话结束仍有未补 next_assistant 的 KG 查询 → 记空标记，便于 LLM 判断"查了但无后续回复"
    for sid, ctx in pending_kg.items():
        if ctx.get("waiting_assistant"):
            ctx["next_assistant"] = ""
            sessions[sid].kg_contexts.append(ctx)

    return sessions


def analyze(s: AuditSession) -> dict:
    """对单个会话做审计判定，返回结构化结论。

    判定基准 (与用户审计清单一一对应):
    - no_kg_query            : 处理了服务/配置/历史类问题却 0 次 KG 检索
    - kg_after_code_read     : 先深度读码后才查 KG (查询时机问题)
    - code_heavy_no_kg_loc   : 大量读码却从未先经 KG 定位 (未复用已构建代码)
    - full_file_reread       : 同一文件被整读 (无 offset 翻页) 反复读取
    - missed_memorize        : 用户说"记住"但未调 dt_memorize
    - missed_history_lookup  : 涉及历史/前情但未先查 world=memory
    """
    findings = []

    # 工具事件索引 (便于查询)
    first_code_read_idx = None
    first_kg_search_idx = None
    first_sense_idx = None
    # 统计读码事件的 offset 分布
    read_counts: Counter = Counter()       # path -> 整读次数 (无 offset)
    read_total: Counter = Counter()        # path -> 总读次数
    search_targets: Counter = Counter()    # path/pattern -> search 次数
    for i, e in enumerate(s.events):
        if e["tool"] in CODE_READ_TOOLS:
            if first_code_read_idx is None:
                first_code_read_idx = i
            if e["tool"] in ("read_file", "read_file_content"):
                # 从 args 判断是否翻页读 (带 offset 视为分段读，非重复)
                is_page = False
                try:
                    a = json.loads(e["args"]) if e["args"].startswith("{") else {}
                    if a.get("offset") is not None:
                        is_page = True
                except Exception:
                    pass
                if not is_page and e["sig"]:
                    read_counts[e["sig"]] += 1
                read_total[e["sig"]] += 1
            elif e["tool"] in ("search_files", "search_file", "search_content", "grep"):
                if e["sig"]:
                    search_targets[e["sig"]] += 1
            elif e["tool"] in ("terminal",):
                if e["sig"]:
                    search_targets[e["sig"]] += 1
        if e["tool"] in KG_SEARCH_TOOLS and first_kg_search_idx is None:
            first_kg_search_idx = i
        if e["tool"] in KG_SENSE_TOOLS and first_sense_idx is None:
            first_sense_idx = i

    # ── 判定1: 是否查询 KG (服务/配置/历史类场景)
    if s.kg_searches == 0 and s.kg_raw_query == 0 and s.kg_sense == 0:
        # 用"会话内是否有深度读码/写代码"辅助判断是否需要 KG
        is_code_session = s.code_reads > 0 or len(read_total) > 0
        # 负载过滤 (P5 修复): 无负载小会话 (消息极少、无代码/历史/服务线索) 属正常，不产生告警
        has_service_or_history_clue = bool(s.history_intent_hits) or bool(s.memorize_intent_hits)
        is_tiny_session = s.msg_count <= 3 and s.code_reads < 1 and not has_service_or_history_clue
        if not is_tiny_session:
            findings.append({
                "id": "no_kg_query",
                "severity": "info" if not is_code_session else "warn",
                "msg": "本会话 0 次 KG 查询。"
                       + ("处理了较多代码读取却未查 KG，若属代码定位场景则违反『先 sense/定位 → 再读码』准则。"
                          if is_code_session else "无代码/服务/历史类负载，不查 KG 属正常。"),
                "evidence": {"kg_searches": s.kg_searches, "kg_sense": s.kg_sense, "code_reads": s.code_reads},
            })

    # ── 判定2: 查询时机是否在深度读码之前
    if first_code_read_idx is not None and first_kg_search_idx is not None:
        if first_kg_search_idx > first_code_read_idx:
            findings.append({
                "id": "kg_after_code_read",
                "severity": "warn",
                "msg": "先深度读码 (idx={first_code}) 之后才查询 KG (idx={first_kg})，违反『先 sense/查询 → 再读源码』准则。".format(
                    first_code=first_code_read_idx, first_kg=first_kg_search_idx),
                "evidence": {
                    "first_code_read": s.events[first_code_read_idx]["tool"],
                    "first_kg_search": s.events[first_kg_search_idx]["tool"],
                },
            })
    elif s.kg_sense == 0 and (read_total or search_targets) and first_kg_search_idx is None:
        # 有读码但从未 dt_sense —— 连环境感知都没做 (对代码会话更关键)
        if s.code_reads >= 5:
            findings.append({
                "id": "no_sense_before_code",
                "severity": "info",
                "msg": f"会话读码 {s.code_reads} 次但从未调用 dt_sense / dt_search_kg(world=code) 先定位，"
                       "可能未复用知识图谱中已构建的代码实体。",
                "evidence": {"code_reads": s.code_reads, "kg_sense": s.kg_sense, "kg_search": s.kg_searches},
            })

    # ── 判定3: 代码大量读取却从未经 KG 定位 (复用已构建代码)
    if (sum(read_total.values()) + sum(search_targets.values())) >= 10 and first_kg_search_idx is None:
        findings.append({
            "id": "code_heavy_no_kg_loc",
            "severity": "warn",
            "msg": f"会话对代码文件读取/搜索 {sum(read_total.values()) + sum(search_targets.values())} 次，"
                   f"却从未先经 dt_search_kg(world=code) 定位。按 AGENTS.md 应先『KG 定位 → 再读源码』，"
                   "否则未复用知识图谱中已构建的代码实体。",
            "evidence": {
                "reads": sum(read_total.values()),
                "searches": sum(search_targets.values()),
                "kg_search": s.kg_searches,
            },
        })

    # ── 判定4: 同一文件整文件反复重读 (无 offset 翻页)
    for sig, cnt in read_counts.items():
        if cnt >= REPEAT_SEARCH_THRESHOLD:
            s.repeated_code_searches.append({"sig": sig, "count": cnt})
            # P2 强化: 高次数 (≥3) 升为 warn，提示未用 KG 定点读
            sev = "warn" if cnt >= 3 else "info"
            findings.append({
                "id": "full_file_reread",
                "severity": sev,
                "msg": f"同一文件被整读 (无翻页 offset) {cnt} 次 (≥{REPEAT_SEARCH_THRESHOLD})：{sig}。"
                       f"可能为真冗余，可先经 KG 定位再精准读取 (offset/limit 定点读)。",
                "evidence": {"sig": sig, "count": cnt},
            })

    # ── 判定5: 是否应 memorize 却没记
    if s.memorize_intent_hits and s.kg_memorize == 0:
        findings.append({
            "id": "missed_memorize",
            "severity": "warn",
            "msg": f"用户触发 {len(s.memorize_intent_hits)} 次记忆意图 (如「{s.memorize_intent_hits[0]['kw']}」) "
                   f"但会话内 0 次 dt_memorize/dt_learn，疑似遗漏记忆。",
            "evidence": {"intents": s.memorize_intent_hits, "memorize_calls": s.kg_memorize},
        })

    # ── 判定6: 是否遗漏历史决策
    if s.history_intent_hits and s.kg_searches == 0 and s.kg_raw_query == 0:
        findings.append({
            "id": "missed_history_lookup",
            "severity": "info",
            "msg": f"用户提及 {len(s.history_intent_hits)} 次历史/前情线索 (如「{s.history_intent_hits[0]['kw']}」) "
                   f"但会话内 0 次 KG 检索，可能遗漏历史决策。",
            "evidence": {"intents": s.history_intent_hits, "kg_searches": s.kg_searches},
        })

    return {
        "session_id": s.session_id,
        "msg_count": s.msg_count,
        "start": parsetime(s.start_ts) if s.start_ts else None,
        "end": parsetime(s.end_ts) if s.end_ts else None,
        "signals": {
            "kg_searches": s.kg_searches,
            "kg_sense": s.kg_sense,
            "kg_memorize": s.kg_memorize,
            "kg_raw_query": s.kg_raw_query,
            "code_reads": s.code_reads,
        },
        "findings": findings,
    }


def build_report(sessions_analysis: list[dict], date: str) -> str:
    """生成人类可读 Markdown 报告。"""
    total_sessions = len(sessions_analysis)
    total_msgs = sum(s["msg_count"] for s in sessions_analysis)
    total_kg = sum(s["signals"]["kg_searches"] for s in sessions_analysis)
    total_sense = sum(s["signals"]["kg_sense"] for s in sessions_analysis)
    total_mem = sum(s["signals"]["kg_memorize"] for s in sessions_analysis)
    total_reads = sum(s["signals"]["code_reads"] for s in sessions_analysis)

    # 过滤有检出项的会话 & 无检出但有行为的会话
    with_findings = [s for s in sessions_analysis if s["findings"]]
    with_kg = [s for s in sessions_analysis if s["signals"]["kg_searches"] or s["signals"]["kg_sense"]]

    lines = []
    lines.append(f"# 每日知识图谱行为审计 — {date}")
    lines.append("")
    lines.append(f"- 审计会话数: **{total_sessions}**")
    lines.append(f"- 消息总数: **{total_msgs}**")
    lines.append(f"- KG 查询 (dt_search_kg): **{total_kg}**")
    lines.append(f"- dt_sense: **{total_sense}**")
    lines.append(f"- dt_memorize/dt_learn: **{total_mem}**")
    lines.append(f"- 代码读取 (read_file/search_files): **{total_reads}**")
    lines.append(f"- 检出问题会话: **{len(with_findings)}** / KG 命中会话: **{len(with_kg)}**")
    lines.append("")

    # 汇总问题类型
    sev_counter = Counter()
    id_counter = Counter()
    for s in with_findings:
        for f in s["findings"]:
            sev_counter[f["severity"]] += 1
            id_counter[f["id"]] += 1
    lines.append("## 问题分布")
    lines.append("")
    lines.append("| 问题类型 | 数量 | 级别 |")
    lines.append("|---|---|---|")
    labels = {
        "no_kg_query": "未查询 KG",
        "kg_after_code_read": "查询时机落后于读码",
        "no_sense_before_code": "读码前未做环境感知",
        "code_heavy_no_kg_loc": "大量读码未先用 KG 定位",
        "full_file_reread": "整文件反复重读",
        "missed_memorize": "疑似遗漏记忆",
        "missed_history_lookup": "疑似遗漏历史决策",
    }
    for fid, cnt in sorted(id_counter.items(), key=lambda x: -x[1]):
        lvl = sev_counter.get(fid.replace("_", "_"), "info")
        # 找该 id 的级别
        for s in with_findings:
            for f in s["findings"]:
                if f["id"] == fid:
                    lvl = f["severity"]
                    break
        lines.append(f"| {labels.get(fid, fid)} | {cnt} | {lvl} |")
    lines.append("")

    # 逐会话详情
    lines.append("## 逐会话详情")
    lines.append("")
    for s in sessions_analysis:
        if not s["findings"]:
            continue
        lines.append(f"### `{s['session_id']}`  ({s['msg_count']} msgs, {s['start']} ~ {s['end']})")
        lines.append("")
        lines.append(f"- 信号: KG查询={s['signals']['kg_searches']}, sense={s['signals']['kg_sense']}, "
                     f"memorize={s['signals']['kg_memorize']}, 读码={s['signals']['code_reads']}")
        for f in s["findings"]:
            lines.append(f"- [{f['severity']}] {f['msg']}")
        lines.append("")

    lines.append("---")
    lines.append("> 注: 本报告由确定性脚本生成，语义裁决 (查询结果是否真正被用、是否该记) 部分为启发式信号，"
                 "建议结合 LLM 复审确认。重复检索阈值 = 2。")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description="每日会话 KG 行为审计")
    ap.add_argument("--date", default=os.environ.get("AUDIT_DATE"),
                    help="目标日期 YYYY-MM-DD (默认: 凌晨审计前一天)。")
    ap.add_argument("--db", default=DEFAULT_DB)
    ap.add_argument("--json", action="store_true", help="输出结构化 JSON")
    ap.add_argument("--semantic-context", action="store_true",
                    help="输出 KG 查询前后上下文，供 LLM 语义复审裁决『结果是否被真正使用』")
    ap.add_argument("--report-out", default=None, help="报告写到路径 (默认 stdout)")
    args = ap.parse_args()

    date = args.date or (dt.date.today() - dt.timedelta(days=1)).isoformat()
    if not os.path.exists(args.db):
        print(f"[ERROR] state.db 不存在: {args.db}", file=sys.stderr)
        return 2

    sessions = load_deviation_events(args.db, date)
    if not sessions:
        print(f"[WARN] {date} 无会话消息，无审计对象。", file=sys.stderr)
        return 0

    analysis = [analyze(s) for s in sessions.values()]
    # 排序: 检出问题多的在前
    analysis.sort(key=lambda s: (-len(s["findings"]), -s["msg_count"]))

    # 语义复审模式: 输出 KG 查询上下文 + 每个会话的发现项，供 LLM 裁决
    if args.semantic_context:
        out = {
            "date": date,
            "note": ("以下为确定性证据，仅含『实际发起过 KG 查询』的会话（这些才需要语义裁决『查询结果是否被真正使用』）。"
                     "无 KG 查询的会话其客观发现已由 Markdown 报告覆盖：若 code_reads 大而 kg_searches=0，"
                     "应判断『代码定位场景是否该查 KG 却未查』。"),
            "sessions": [],
        }
        sid_map = {s.session_id: s for s in sessions.values()}
        for a in analysis:
            sid = a["session_id"]
            ctx = sid_map[sid].kg_contexts
            # 只有实际发起过 KG 查询的会话才需要 LLM 看上下文裁决
            if not ctx:
                continue
            entry = {
                "session_id": sid,
                "msg_count": a["msg_count"],
                "signals": a["signals"],
                "findings": a["findings"],
                "kg_queries": ctx,
            }
            out["sessions"].append(entry)
        print(json.dumps(out, ensure_ascii=False, indent=2))
        return 0

    if args.json:
        print(json.dumps({"date": date, "sessions": analysis}, ensure_ascii=False, indent=2))
        return 0

    report = build_report(analysis, date)
    if args.report_out:
        Path(args.report_out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.report_out).write_text(report, encoding="utf-8")
        print(report)
    else:
        print(report)
    return 0


if __name__ == "__main__":
    sys.exit(main())
