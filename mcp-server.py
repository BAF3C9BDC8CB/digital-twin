#!/home/luis/.local/miniconda3/bin/python3
"""
DT MCP Server — 将 digital-twin CLI 命令注册为 OpenCode Tool


提供工具:
  dt_search_kg       → dt search-kg (知识图谱向量搜索)
  dt_search_expand   → dt search --expand (代码查询扩展搜索)
  dt_build           → dt build --path/--file (项目/文件索引，支持多路径)
  svc_list/status/logs/start/stop/restart  → 本地微服务管理
  kublog_status/logs/download              → K8s 日志与状态
  jcli_list/params/history/build_log/build → Jenkins 部署
"""

import json
import os
import re
import subprocess
import sys
from datetime import datetime
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import Tool, TextContent

server = Server("digital-twin")
LOG_FILE = "/tmp/digital-twin-mcp.log"


def log_tool(name: str, args: dict, ts_start: datetime, ts_end: datetime, output: str):
    """写入工具调用详情到日志文件"""
    duration_ms = (ts_end - ts_start).total_seconds() * 1000
    ts = ts_start.strftime("%Y-%m-%d %H:%M:%S")
    entry = (
        f"{'='*60}\n"
        f"时间: {ts}\n"
        f"工具: {name}\n"
        f"参数: {json.dumps(args, ensure_ascii=False, default=str)}\n"
        f"耗时: {duration_ms:.0f}ms\n"
        f"输出:\n{output}\n"
        f"{'='*60}\n"
    )
    print(f"[MCP] ✓ {name} ({duration_ms:.0f}ms)", file=sys.stderr, flush=True)
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(entry + "\n")
    except Exception:
        pass  # 日志写入失败不影响工具执行


def run_cmd(cmd: list, timeout: int = 120) -> str:
    """执行命令并返回 stdout，清理 ANSI 转义序列"""
    print(f"[CMD] {' '.join(cmd)}", file=sys.stderr, flush=True)
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    output = (result.stdout + result.stderr).strip()
    # 清理 ANSI 颜色码
    output = re.sub(r'\x1b\[[0-9;]*m', '', output)
    return output or "(无输出)"


# ====== 工具注册 ======

@server.list_tools()
async def list_tools():
    return [
        # --- dt ---
        Tool(
            name="dt_search_kg",
            description="向量语义搜索知识图谱节点(无需写Cypher)。返回匹配的KG节点及其elementId。拿到elementId后用neo4j_read_cypher精确取完整属性。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "自然语言搜索关键词"},
                    "limit": {"type": "integer", "description": "返回数量", "default": 10}
                }, "required": ["query"]
            }
        ),
        Tool(
            name="dt_search_expand",
            description="扩展语义代码搜索(多查询变体合并去重)。低级模型推荐使用。通过 path 或 name 限定搜索范围。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "搜索关键词"},
                    "path": {"type": "string", "description": "搜索范围路径(优先)，搜索该路径下所有项目，通常传当前工作目录即可"},
                    "name": {"type": "string", "description": "搜索范围项目名(path 未传时使用)"},
                    "limit": {"type": "integer", "description": "返回数量", "default": 10}
                }, "required": ["query"]
            }
        ),

        # --- svc: 本地微服务管理 ---
        Tool(
            name="svc_list",
            description="列出所有本地微服务及运行状态",
            inputSchema={
                "type": "object",
                "properties": {},
                "required": []
            }
        ),
        Tool(
            name="svc_status",
            description="查看指定微服务的详细状态",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "服务名称"}
                }, "required": ["name"]
            }
        ),
        Tool(
            name="svc_logs",
            description="查看指定微服务的运行日志",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "服务名称"},
                    "lines": {"type": "integer", "description": "显示行数", "default": 50}
                }, "required": ["name"]
            }
        ),
        Tool(
            name="svc_start",
            description="启动指定的本地微服务(编译+启动)",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "服务名称"}
                }, "required": ["name"]
            }
        ),
        Tool(
            name="svc_stop",
            description="停止指定的本地微服务",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "服务名称"}
                }, "required": ["name"]
            }
        ),
        Tool(
            name="svc_restart",
            description="重启指定的本地微服务",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "服务名称"}
                }, "required": ["name"]
            }
        ),

        # --- kublog: K8s 日志与状态 ---
        Tool(
            name="kublog_status",
            description="查看K8s集群中Pod/Deployment/Service的状态(替代kubectl)",
            inputSchema={
                "type": "object",
                "properties": {
                    "resource": {
                        "type": "string",
                        "description": "资源类型: pods / deploy / svc",
                        "enum": ["pods", "deploy", "svc"]
                    },
                    "namespace": {"type": "string", "description": "命名空间", "default": "default"}
                }, "required": ["resource"]
            }
        ),
        Tool(
            name="kublog_logs",
            description="实时查看/监听K8s Pod日志(解决了Kuboard网页日志断开问题)",
            inputSchema={
                "type": "object",
                "properties": {
                    "pod": {"type": "string", "description": "Pod名称"},
                    "namespace": {"type": "string", "description": "命名空间", "default": "default"},
                    "since": {"type": "string", "description": "回溯时间，如 30m / 2h"},
                    "previous": {"type": "boolean", "description": "查看重启前的日志", "default": False}
                }, "required": ["pod"]
            }
        ),
        Tool(
            name="kublog_download",
            description="下载K8s Pod日志到本地文件",
            inputSchema={
                "type": "object",
                "properties": {
                    "pod": {"type": "string", "description": "Pod名称"},
                    "namespace": {"type": "string", "description": "命名空间", "default": "default"},
                    "since": {"type": "string", "description": "回溯时间，如 1h / 24h"},
                    "output": {"type": "string", "description": "输出文件路径"}
                }, "required": ["pod"]
            }
        ),

        # --- jcli: Jenkins 部署 ---
        Tool(
            name="jcli_list",
            description="列出所有Jenkins Job",
            inputSchema={
                "type": "object", "properties": {}, "required": []
            }
        ),
        Tool(
            name="jcli_params",
            description="查看Jenkins Job的参数定义",
            inputSchema={
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Job名称"}
                }, "required": ["job"]
            }
        ),
        Tool(
            name="jcli_history",
            description="查看Jenkins Job的构建历史",
            inputSchema={
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Job名称"},
                    "limit": {"type": "integer", "description": "显示条数", "default": 10}
                }, "required": ["job"]
            }
        ),
        Tool(
            name="jcli_build_log",
            description="查看Jenkins Job的构建日志",
            inputSchema={
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Job名称"},
                    "build": {"type": "string", "description": "构建编号(默认取最新)"}
                }, "required": ["job"]
            }
        ),
        Tool(
            name="jcli_build",
            description="触发Jenkins Job构建(⚠️ 仅当用户明确要求发布时使用。测试环境默认，明确说正式/生产才传production)",
            inputSchema={
                "type": "object",
                "properties": {
                    "job": {"type": "string", "description": "Job名称"},
                    "params": {"type": "string", "description": "构建参数 KEY=VALUE (逗号分隔)"},
                    "env": {
                        "type": "string",
                        "description": "环境: test(默认) / production",
                        "enum": ["test", "production"],
                        "default": "test"
                    }
                }, "required": ["job"]
            }
        ),

        # --- dt sync ---
        Tool(
            name="nacos_sync",
            description="同步Nacos配置到知识图谱(修改Nacos配置后应触发此同步)",
            inputSchema={
                "type": "object",
                "properties": {
                    "env": {
                        "type": "string",
                        "description": "环境: test / prod / all",
                        "enum": ["test", "prod", "all"],
                        "default": "all"
                    }
                }, "required": []
            }
        ),

        # --- dt: 维护 ---
        Tool(
            name="dt_health",
            description="检查所有后端服务健康状态(Neo4j/Embed/Qdrant/KG Bridge/Fulltext)",
            inputSchema={"type": "object", "properties": {}, "required": []}
        ),
        Tool(
            name="dt_kg_sync",
            description="同步KG节点到Qdrant向量库(KG→Qdrant桥接)。KG节点变更后应触发增量同步。",
            inputSchema={
                "type": "object",
                "properties": {
                    "incremental": {"type": "boolean", "description": "仅同步未索引节点", "default": False},
                    "labels": {"type": "string", "description": "指定标签(逗号分隔)，默认全部业务标签"}
                }, "required": []
            }
        ),
        Tool(
            name="dt_memorize",
            description="写入知识节点到KG(架构决策、用户说'记住')。--type: Decision/KnowledgeAdded/Environment/Dependencies",
            inputSchema={
                "type": "object",
                "properties": {
                    "type": {"type": "string", "description": "知识类型"},
                    "entity_id": {"type": "string", "description": "唯一标识"},
                    "entity_type": {"type": "string", "description": "实体类型"},
                    "project": {"type": "string", "description": "所属项目"},
                    "details": {"type": "string", "description": "详细内容"}
                }, "required": ["type", "entity_id", "details"]
            }
        ),
        Tool(
            name="dt_event",
            description="写入事件节点到KG(部署/安装/配置变更/会话记录)。--type: Deploy/SoftwareInstalled/ConfigChange/Conversation",
            inputSchema={
                "type": "object",
                "properties": {
                    "type": {"type": "string", "description": "事件类型"},
                    "entity_id": {"type": "string", "description": "唯一标识"},
                    "entity_type": {"type": "string", "description": "实体类型"},
                    "project": {"type": "string", "description": "所属项目"},
                    "details": {"type": "string", "description": "详细内容"}
                }, "required": ["type", "entity_id", "details"]
            }
        ),
        Tool(
            name="dt_build",
            description="增量构建：扫描项目，hash 对比文件，仅索引变更部分。path 支持项目目录或文件绝对路径，传文件时自动解析项目名。",
            inputSchema={
                "type": "object",
                "properties": {
                    "path": {
                        "oneOf": [
                            {"type": "string", "description": "项目根路径或文件绝对路径"},
                            {"type": "array", "items": {"type": "string"}, "description": "多个路径/文件"}
                        ],
                        "description": "项目根路径或文件绝对路径，支持单个字符串或字符串数组"
                    },
                    "name": {"type": "string", "description": "项目名称(传目录时必填，传文件时自动解析)"}
                }, "required": ["path"]
            }
        ),
    ]


# ====== 工具调用 ======

@server.call_tool()
async def call_tool(name: str, arguments: dict):
    ts_start = datetime.now()
    args_brief = json.dumps(arguments, ensure_ascii=False, default=str)
    print(f"[MCP] ▶ {name} {args_brief}", file=sys.stderr, flush=True)
    text = ""

    # --- dt ---
    if name == "dt_search_kg":
        query = arguments.get("query", "")
        limit = arguments.get("limit", 10)
        text = run_cmd(["dt", "search-kg", query, "--limit", str(limit)])

    elif name == "dt_search_expand":
        query = arguments.get("query", "")
        path = arguments.get("path", "")
        project = arguments.get("name", "")
        limit = arguments.get("limit", 10)
        cmd = ["dt", "search", query, "--expand", "--json", "--limit", str(limit)]
        if path:
            cmd += ["--path", path]
        elif project:
            cmd += ["--project", project]
        print(f"[CMD] {' '.join(cmd)}", file=sys.stderr, flush=True)
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        if result.returncode != 0:
            text = f"错误: {result.stderr.strip()}"
        else:
            try:
                data = json.loads(result.stdout)
                lines = []
                for r in data:
                    score = r.get("score", 0)
                    name = r.get("name", "?")
                    fp = r.get("file_path", "?")
                    sl = r.get("start_line", "?")
                    sig = r.get("signature", "")[:120].replace("\n", " ")
                    lines.append(f"[{score:.3f}] {name} ({fp}:{sl})\n    {sig}...")
                text = "\n\n".join(lines)
            except (json.JSONDecodeError, KeyError):
                text = result.stdout.strip()

    # --- svc ---
    elif name == "svc_list":
        text = run_cmd(["svc", "list"])
    elif name == "svc_status":
        text = run_cmd(["svc", "status", arguments["name"]])
    elif name == "svc_logs":
        text = run_cmd(["svc", "logs", arguments["name"], "--lines", str(arguments.get("lines", 50))])
    elif name == "svc_start":
        text = run_cmd(["svc", "start", arguments["name"]], timeout=300)
    elif name == "svc_stop":
        text = run_cmd(["svc", "stop", arguments["name"]])
    elif name == "svc_restart":
        text = run_cmd(["svc", "restart", arguments["name"]], timeout=300)

    # --- kublog ---
    elif name == "kublog_status":
        res = arguments["resource"]
        ns = arguments.get("namespace", "default")
        text = run_cmd(["kublog", "status", res, "--ns", ns])
    elif name == "kublog_logs":
        pod = arguments["pod"]
        ns = arguments.get("namespace", "default")
        cmd = ["kublog", "logs", "--ns", ns, "--pod", pod, "--no-follow"]
        if arguments.get("since"):
            cmd += ["--since", arguments["since"]]
        if arguments.get("previous"):
            cmd += ["--previous"]
        text = run_cmd(cmd)
    elif name == "kublog_download":
        pod = arguments["pod"]
        ns = arguments.get("namespace", "default")
        cmd = ["kublog", "download", "--ns", ns, pod]
        if arguments.get("since"):
            cmd += ["--since", arguments["since"]]
        if arguments.get("output"):
            cmd += ["-o", arguments["output"]]
        text = run_cmd(cmd, timeout=300)

    # --- jcli ---
    elif name == "jcli_list":
        text = run_cmd(["jcli", "jobs"])
    elif name == "jcli_params":
        text = run_cmd(["jcli", "params", arguments["job"]])
    elif name == "jcli_history":
        limit = arguments.get("limit", 10)
        text = run_cmd(["jcli", "history", arguments["job"], "-n", str(limit)])
    elif name == "jcli_build_log":
        cmd = ["jcli", "log", arguments["job"]]
        if arguments.get("build"):
            cmd.append(arguments["build"])
        text = run_cmd(cmd)
    elif name == "jcli_build":
        env = arguments.get("env", "test")
        cmd = ["jcli", "build", arguments["job"]]
        if arguments.get("params"):
            for p in arguments["params"].split(","):
                cmd += ["-p", p.strip()]
        if env == "production":
            cmd += ["--production"]
        text = run_cmd(cmd, timeout=600)

    # --- dt sync ---
    elif name == "nacos_sync":
        env = arguments.get("env", "all")
        text = run_cmd(["dt", "nacos-sync", "--env", env], timeout=300)

    # --- dt: 维护 ---
    elif name == "dt_health":
        text = run_cmd(["dt", "health"])
    elif name == "dt_kg_sync":
        cmd = ["dt", "kg-sync"]
        if arguments.get("incremental"):
            cmd.append("--incremental")
        if arguments.get("labels"):
            for lbl in arguments["labels"].split(","):
                cmd += ["--labels", lbl.strip()]
        text = run_cmd(cmd, timeout=300)
    elif name == "dt_memorize":
        cmd = ["dt", "memorize", "--type", arguments["type"], "--entity-id", arguments["entity_id"]]
        if arguments.get("entity_type"): cmd += ["--entity-type", arguments["entity_type"]]
        if arguments.get("project"): cmd += ["--project", arguments["project"]]
        cmd += ["--details", arguments.get("details", "")]
        text = run_cmd(cmd)
    elif name == "dt_event":
        cmd = ["dt", "event", "--type", arguments["type"], "--entity-id", arguments["entity_id"]]
        if arguments.get("entity_type"): cmd += ["--entity-type", arguments["entity_type"]]
        if arguments.get("project"): cmd += ["--project", arguments["project"]]
        cmd += ["--details", arguments.get("details", "")]
        text = run_cmd(cmd)
    elif name == "dt_build":
        paths = arguments.get("path", [])
        if isinstance(paths, str):
            paths = [paths]
        project_name = arguments.get("name", "")
        results = []
        for p in paths:
            if os.path.isfile(p):
                r = run_cmd(["dt", "build", "--file", p])
            else:
                cmd = ["dt", "build", "--path", p]
                if project_name:
                    cmd += ["--name", project_name]
                r = run_cmd(cmd, timeout=300)
            results.append(r)
        text = "\n".join(results)

    else:
        text = f"未知工具: {name}"

    ts_end = datetime.now()
    log_tool(name, arguments, ts_start, ts_end, text)
    return [TextContent(type="text", text=text)]


async def main():
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream,
            write_stream,
            server.create_initialization_options()
        )


if __name__ == "__main__":
    import asyncio
    asyncio.run(main())
