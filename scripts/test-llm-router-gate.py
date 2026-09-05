#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
验证「LLM 路由决策」可行性 —— 把 dt search/router 的两层规则替换为 LLM 判断：

  阶段1 门控 (gate)   : LLM 输出 {"search": true/false, "reason": "..."}
                        决定"是否值得发起搜索"(替代 should_search + 闲聊词表)
  阶段2 路由 (route)  : LLM 输出 {"world": "code|knowledge|doc|config|memory|all"}
                        决定"搜哪个世界"(替代 analyze_query_intent + build_route)
  阶段3 过滤 (filter) : LLM 对每条命中输出 {"relevant": true/false}
                        决定"这条结果是否相关"(替代 judge_relevance 文本解析)

不读取/打印 api_key 值；只从 config/pipeline.yaml 读取调用所需字段。
用法: python3 scripts/test-llm-router-gate.py
"""
import json
import os
import sys
import time

import yaml

# ---------------------------------------------------------------------------
# 配置读取（与 scripts/test-llm-quick.py 同范式, 只取字段名不打印值）
# ---------------------------------------------------------------------------
CONFIG_PATH = os.path.join(os.path.dirname(__file__), "..", "config", "pipeline.yaml")


def load_llm_config():
    with open(CONFIG_PATH, "r", encoding="utf-8") as f:
        cfg = yaml.safe_load(f)
    llm = cfg.get("llm", {})
    sf = cfg["providers"]["siliconflow"]
    return {
        "url": sf["url"].rstrip("/") + "/chat/completions",
        "api_key": sf["api_key"],  # 仅用于请求头, 不打印
        "model": llm.get("model", "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B"),
        "temperature": llm.get("temperature", 0.0),
    }


LLM_CFG = load_llm_config()


def llm_chat(system: str, user: str, max_tokens: int = 300, timeout: int = 30) -> str:
    """单次 LLM 调用, 返回原始文本。失败抛异常由调用方处理。"""
    import requests

    resp = requests.post(
        LLM_CFG["url"],
        headers={
            "Authorization": f"Bearer {LLM_CFG['api_key']}",
            "Content-Type": "application/json",
        },
        json={
            "model": LLM_CFG["model"],
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
            "temperature": LLM_CFG["temperature"],
            "max_tokens": max_tokens,
            "stream": False,
        },
        timeout=timeout,
    )
    resp.raise_for_status()
    data = resp.json()
    return data["choices"][0]["message"]["content"].strip()


def parse_json_strict(text: str) -> dict:
    """解析 LLM 输出为 JSON。容忍 ```json 围栏与前后杂讯, 失败抛异常。"""
    t = text.strip()
    # 去掉 markdown 代码围栏
    if t.startswith("```"):
        t = t.split("\n", 1)[1]
        t = t.rsplit("```", 1)[0]
    # 找第一个 { 到最后一个 } 之间的内容
    start, end = t.find("{"), t.rfind("}")
    if start == -1 or end == -1 or end <= start:
        raise ValueError(f"无 JSON 对象: {text[:120]!r}")
    return json.loads(t[start : end + 1])


# ---------------------------------------------------------------------------
# 阶段1 门控 prompt: 判断是否值得搜索
# ---------------------------------------------------------------------------
GATE_SYSTEM = """你是代码库检索助手的前置闸门。用户发来一句话, 你要判断: 这句话是否值得触发一次代码/文档/知识库检索?

判定规则:
1. search=true (值得检索): 查询包含任何具体可检索对象 —— 代码符号(类名/方法名/变量名)、文件名或路径、配置项、业务概念(支付/订单/服务/接口/幂等)、技术术语、报错信息、或明确指向"某个东西在哪/怎么用/为什么/是什么"的检索意图。
2. search=false (不值得检索): 纯寒暄/问候/道谢/闲聊(你好/谢谢/在吗/天气不错)、纯任务指令且无检索对象(帮我实现/给我建议/介绍一下有哪些模块——除非提到了具体模块名)、纯算术、与代码库无关的话题。
3. 拿不准时倾向 search=true(宁可多搜一次, 不要漏掉真实需求)。

只输出 JSON, 不要任何解释, 格式:
{"search": true或false, "reason": "10字以内理由"}"""

GATE_CASES = [
    # (查询, 期望, 说明)
    ("你好", False, "寒暄"),
    ("谢谢", False, "道谢"),
    ("天气怎么样", False, "闲聊"),
    ("帮我算一下 1+1", False, "纯算术"),
    ("好的 收到", False, "应答"),
    ("how are you", False, "英文寒暄"),
    ("帮我实现", False, "任务性无锚点"),
    ("给我一些建议", False, "任务性无锚点"),
    ("介绍一下有哪些模块", False, "无具体对象"),
    ("有什么问题", False, "泛指无对象"),
    ("MemgraphClient", True, "类名(强锚点)"),
    ("connect_memgraph", True, "方法名(强锚点)"),
    ("支付超时怎么配置", True, "业务概念+配置意图"),
    ("config/datasource.yaml", True, "文件路径"),
    ("payment callback 幂等逻辑在哪", True, "业务术语+定位意图"),
    ("帮我实现一个轮询功能", True, "任务+具体对象"),
    ("这个功能怎么用", True, "检索意图(边界)"),
    ("之前那个订单重复扣款的问题", True, "业务上下文(边界)"),
    ("今天吃了吗", False, "生活闲聊(边界)"),
    ("CrossWorldSearchTrait 是干嘛的", True, "标识符+意图"),
]


def test_gate() -> dict:
    """验证阶段1: LLM 门控是否可靠地区分"该搜/不该搜"。"""
    print("=" * 70)
    print("阶段1 门控测试: LLM 判断是否值得搜索 (search: true/false)")
    print("=" * 70)
    ok = 0
    fail = 0
    total_ms = 0.0
    for query, expect, note in GATE_CASES:
        t0 = time.time()
        try:
            raw = llm_chat(
                GATE_SYSTEM,
                f"用户说: {query}\n\n请判断。",
                max_tokens=150,
            )
            parsed = parse_json_strict(raw)
            got = bool(parsed.get("search"))
            ms = (time.time() - t0) * 1000
            total_ms += ms
            mark = "✓" if got == expect else "✗"
            if got == expect:
                ok += 1
            else:
                fail += 1
            print(
                f"{mark} [{note:10s}] {query[:24]:<26s} 期望={str(expect):5s} 实际={str(got):5s} "
                f"({ms:.0f}ms) reason={parsed.get('reason','')}"
            )
        except Exception as e:
            fail += 1
            ms = (time.time() - t0) * 1000
            total_ms += ms
            print(f"✗ [{note:10s}] {query[:24]:<26s} 解析失败: {e}")
    n = len(GATE_CASES)
    print(f"\n阶段1 结果: {ok}/{n} 通过 ({ok/n*100:.0f}%), 平均 {total_ms/n:.0f}ms/次")
    return {"ok": ok, "total": n, "avg_ms": total_ms / n}


# ---------------------------------------------------------------------------
# 阶段2 路由 prompt: 判断搜哪个世界
# ---------------------------------------------------------------------------
ROUTE_SYSTEM = """你是代码库检索路由。根据查询内容, 决定检索哪个数据世界:

- code: 查代码实体(类/方法/模块/接口实现)——查询含代码符号、函数、类、文件、业务逻辑实现位置
- knowledge: 查知识记忆(架构决策/踩坑/经验/概念解释)——查询是"如何/为什么/怎么做/之前怎么处理"且不指向具体代码位置
- doc: 查文档(需求/设计/README/接口文档)——查询含"文档/说明/手册/readme"或明确指向文档内容
- config: 查配置(数据源/中间件/参数)——查询含"配置/数据源/超时参数/nacos/yaml配置项"
- memory: 查会话/用户记忆(个人偏好/历史决定)
- all: 跨世界综合检索(查询宽泛, 不确定具体类型, 或希望全面)

优先规则: 查询含具体代码符号或文件名 → code; 含业务实体但不含代码符号且是"为什么/怎么办" → knowledge;
含文档字样 → doc; 纯配置项 → config; 都像 → all。
只输出 JSON: {"world": "code|knowledge|doc|config|memory|all", "reason": "10字以内理由"}"""

ROUTE_CASES = [
    # (查询, 期望, 说明)
    ("MemgraphClient", "code", "类名"),
    ("connect_memgraph 在哪定义", "code", "方法定位"),
    ("支付超时怎么配置", "config", "配置意图"),
    ("为什么订单会重复扣款", "knowledge", "经验归因"),
    ("接口幂等怎么实现", "knowledge", "方案咨询"),
    ("payment callback 逻辑", "code", "业务代码"),
    ("需求文档里支付流程怎么写的", "doc", "文档"),
    ("nacos 里数据源配置", "config", "配置中心"),
    ("dt search 默认搜哪些世界", "doc", "README 型"),
    ("上次部署用的什么参数", "memory", "历史记录"),
]


def test_route() -> dict:
    print("\n" + "=" * 70)
    print("阶段2 路由测试: LLM 判断检索哪个世界 (world)")
    print("=" * 70)
    ok = 0
    total_ms = 0.0
    n = len(ROUTE_CASES)
    for query, expect, note in ROUTE_CASES:
        t0 = time.time()
        try:
            raw = llm_chat(
                ROUTE_SYSTEM,
                f"查询: {query}\n\n请路由。",
                max_tokens=150,
            )
            parsed = parse_json_strict(raw)
            got = str(parsed.get("world", ""))
            ms = (time.time() - t0) * 1000
            total_ms += ms
            mark = "✓" if got == expect else "✗"
            if got == expect:
                ok += 1
            print(
                f"{mark} [{note:10s}] {query[:24]:<26s} 期望={expect:10s} 实际={got:10s} "
                f"({ms:.0f}ms) reason={parsed.get('reason','')}"
            )
        except Exception as e:
            print(f"✗ [{note:10s}] {query[:24]:<26s} 解析失败: {e}")
    print(f"\n阶段2 结果: {ok}/{n} 通过 ({ok/n*100:.0f}%), 平均 {total_ms/n:.0f}ms/次")
    return {"ok": ok, "total": n, "avg_ms": total_ms / n}


# ---------------------------------------------------------------------------
# 阶段3 结果过滤 prompt: 判断单条命中是否相关
# ---------------------------------------------------------------------------
FILTER_SYSTEM = """你是搜索结果相关性评估专家。根据用户查询和单条搜索结果, 判断它是否真正相关。

判定规则:
1. 代码类命中(Method/Class): 查询只要涉及该文件/方法/类所在的项目、路径、文件、业务领域, 或方法名与查询关键词一致 → relevant=true。仅撞上通用动词方法名(help/execute/get)但业务无关 → relevant=false。
2. 查询只含通用意图词(帮我实现/建议一下), 不含具体对象 → 任何命中都是多余检索 → relevant=false。
3. 文档/知识类命中: 摘要/原文与查询对象(业务概念/配置项/技术术语)语义相关即 true; 仅字符串表面相同但语义无关 → false。
4. 拿不准倾向 relevant=true(宁可多留, 不误删)。

只输出 JSON: {"relevant": true或false, "reason": "15字以内理由"}"""

# 用真实 dt search 输出构造的命中样本
FILTER_CASES = [
    # (query, hit_context, 期望, 说明)
    (
        "MemgraphClient",
        "标题: MemgraphClient | 项目: digital-twin-v2 | 来源世界: code | 实体类型: Class | "
        "文件路径: src/infrastructure/memgraph/client.rs | 行号: L26-28 | "
        "方法分析: 用途:MemgraphClient 结构体封装了Memgraph图数据库的客户端连接",
        True,
        "精确命中",
    ),
    (
        "MemgraphClient",
        "标题: connect_memgraph | 项目: digital-twin-v2 | 来源世界: code | 实体类型: Method | "
        "文件路径: src/runtime.rs | 行号: L268-285 | 签名: fn connect_memgraph() -> Option<...> | "
        "方法分析: 用途: 尝试连接 Memgraph 图数据库服务",
        True,
        "相关方法",
    ),
    (
        "connect_memgraph",
        "标题: MemgraphClient | 项目: digital-twin-v2 | 来源世界: code | 实体类型: Class | "
        "文件路径: src/infrastructure/memgraph/client.rs | "
        "方法分析: 用途:MemgraphClient 结构体封装了Memgraph图数据库的客户端连接",
        True,
        "类与连接方法相关",
    ),
    (
        "支付超时怎么配置",
        "标题: handlePaymentTimeout | 项目: order-center | 来源世界: code | 实体类型: Method | "
        "文件路径: src/service/PaymentService.java | 方法分析: 用途: 处理支付超时后的订单状态流转与补偿逻辑",
        True,
        "业务相关",
    ),
    (
        "支付超时怎么配置",
        "标题: TimeoutConfig | 项目: order-center | 来源世界: code | 实体类型: Class | "
        "文件路径: src/config/TimeoutConfig.java | 方法分析: 用途: 支付超时时间参数的配置类, 定义超时阈值常量",
        True,
        "配置相关",
    ),
    (
        "支付超时怎么配置",
        "标题: UserLoginService | 项目: user-center | 来源世界: code | 实体类型: Class | "
        "文件路径: src/service/UserLoginService.java | 方法分析: 用途: 处理用户登录鉴权与 token 签发",
        False,
        "无关服务",
    ),
    (
        "MemgraphClient",
        "标题: UserLoginService | 项目: user-center | 来源世界: code | 实体类型: Class | "
        "文件路径: src/service/UserLoginService.java | 方法分析: 用途: 处理用户登录鉴权与 token 签发",
        False,
        "完全无关",
    ),
    (
        "MemgraphClient",
        "标题: execute | 项目: digital-twin-v2 | 来源世界: code | 实体类型: Method | "
        "文件路径: src/infrastructure/memgraph/client.rs | 方法分析: 用途: 通用动词方法, 执行查询",
        True,
        "撞通用动词但同文件(边界)",
    ),
]


def test_filter() -> dict:
    print("\n" + "=" * 70)
    print("阶段3 结果过滤测试: LLM 判断单条命中是否相关 (relevant)")
    print("=" * 70)
    ok = 0
    total_ms = 0.0
    n = len(FILTER_CASES)
    for query, hit, expect, note in FILTER_CASES:
        t0 = time.time()
        try:
            raw = llm_chat(
                FILTER_SYSTEM,
                f"用户查询: {query}\n\n单条搜索结果:\n{hit}\n\n请判断。",
                max_tokens=150,
            )
            parsed = parse_json_strict(raw)
            got = bool(parsed.get("relevant"))
            ms = (time.time() - t0) * 1000
            total_ms += ms
            mark = "✓" if got == expect else "✗"
            if got == expect:
                ok += 1
            print(
                f"{mark} [{note:10s}] query={query[:20]:<22s} 期望={str(expect):5s} 实际={str(got):5s} "
                f"({ms:.0f}ms) reason={parsed.get('reason','')}"
            )
        except Exception as e:
            print(f"✗ [{note:10s}] query={query[:20]:<22s} 解析失败: {e}")
    print(f"\n阶段3 结果: {ok}/{n} 通过 ({ok/n*100:.0f}%), 平均 {total_ms/n:.0f}ms/次")
    return {"ok": ok, "total": n, "avg_ms": total_ms / n}


def test_json_stability() -> dict:
    """JSON 解析稳定性: 同一查询跑 5 次, 看输出格式是否稳定(能否被 parse_json_strict 解析)。"""
    print("\n" + "=" * 70)
    print("稳定性测试: 同一查询重复 5 次, 验证 JSON 输出格式稳定可解析")
    print("=" * 70)
    stable = 0
    for i in range(5):
        try:
            raw = llm_chat(GATE_SYSTEM, f"用户说: 支付超时怎么配置\n\n请判断。", max_tokens=150)
            parsed = parse_json_strict(raw)
            stable += 1
            print(f"  第{i+1}次: search={parsed.get('search')} reason={parsed.get('reason','')}")
        except Exception as e:
            print(f"  第{i+1}次: 解析失败 {e}")
    print(f"\n稳定性: {stable}/5 次可解析")
    return {"stable": stable, "total": 5}


def cost_estimate():
    """估算成本: 按 token 计。"""
    print("\n" + "=" * 70)
    print("成本估算(每查询新增 LLM 调用)")
    print("=" * 70)
    print("  门控(gate):  ~150 token 输出 + ~200 token 输入 ≈ 350 token/次")
    print("  路由(route): ~100 token 输出 + ~150 token 输入 ≈ 250 token/次")
    print("  过滤(filter): ~100 token/条, 10 条命中 ≈ 1000+ token/次")
    print("  若 gate+route+filter 全开: 每次搜索 ≈ 1 次 gate + 1 次 route + N 次 filter")
    print("  关键代价不是 token, 是延迟: 每阶段 2-15s(取决于网关), 全开会叠加")


def main():
    r1 = test_gate()
    r2 = test_route()
    r3 = test_filter()
    r4 = test_json_stability()
    cost_estimate()

    print("\n" + "=" * 70)
    print("汇总")
    print("=" * 70)
    g1 = r1["ok"] / r1["total"]
    g2 = r2["ok"] / r2["total"]
    g3 = r3["ok"] / r3["total"]
    print(f"  阶段1 门控: {r1['ok']}/{r1['total']} ({g1*100:.0f}%) 平均 {r1['avg_ms']:.0f}ms/次")
    print(f"  阶段2 路由: {r2['ok']}/{r2['total']} ({g2*100:.0f}%) 平均 {r2['avg_ms']:.0f}ms/次")
    print(f"  阶段3 过滤: {r3['ok']}/{r3['total']} ({g3*100:.0f}%) 平均 {r3['avg_ms']:.0f}ms/次")
    print(f"  JSON 稳定性: {r4['stable']}/{r4['total']}")
    if g1 >= 0.9 and g2 >= 0.8 and g3 >= 0.85 and r4["stable"] >= 4:
        print("\n结论: LLM 路由决策可行, 建议进入 Rust 集成设计")
    else:
        print("\n结论: 部分用例未达预期, 需调整 prompt 或验证集后重测")


if __name__ == "__main__":
    main()
