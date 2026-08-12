#!/home/luis/.local/miniconda3/bin/python3
"""
DT MCP Server V2 — 将 digital-twin CLI 命令注册为 OpenCode Tool

提供工具 (24个):
  搜索: dt_search_kg, dt_search, dt_sense
  知识: dt_memorize, dt_event, dt_learn
  管线: dt_build, dt_kg_sync
  服务: svc_list, svc_status, svc_logs, svc_start, svc_stop, svc_restart
  K8s:  kublog_status, kublog_logs, kublog_download
  Jenkins: jcli_list, jcli_params, jcli_history, jcli_build_log, jcli_build
  运维: dt_health, dt_backup

## 通信架构

当前: 所有工具通过 `subprocess.run()` 调用 CLI 二进制 (DT_BIN/svc/kub/jcli)。
目标: 所有工具通过 gRPC client 调用 dt-daemon 的 plugin service。

迁移计划:
  1. Phase 1 (当前): MCP Server → subprocess → CLI binary  (现状)
  2. Phase 2 (未来): MCP Server → grpcio/grpclib → dt-daemon:50051
      各工具映射到 proto 定义:
        svc_*       → dt.plugin.svc.SvcPlugin
        kublog_*    → dt.plugin.k8s.K8sPlugin
        jcli_*      → dt.plugin.jenkins.JenkinsPlugin
        dt_*        → dt.core.DtCore
  3. Phase 3 (最终): 删除 subprocess 调用，纯 gRPC
"""

import json
import logging
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


# ====== DT_BIN 路径解析 ======

def _resolve_dt_bin() -> str:
    """解析 dt binary 路径，依次尝试多个候选"""
    candidates = [
        os.path.join(os.path.dirname(os.path.abspath(__file__)), "target", "release", "dt"),
        "/data/myProject/digital-twin-v2/target/release/dt",
        "dt",
    ]
    for c in candidates:
        if c == "dt":
            # PATH 中的 dt，不做文件检查
            return c
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return "dt"


DT_BIN = _resolve_dt_bin()

# dt 读取 pipeline 配置的相对路径为 config/pipeline.yaml(基于 cwd)。
# MCP 进程的工作目录不固定,必须让子进程在项目根下执行,
# 否则 providers 配置(xinference)加载不到,embed 会回退到默认 siliconflow。
if "target/release" in DT_BIN:
    _DT_PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(DT_BIN)))
else:
    _DT_PROJECT_ROOT = "/data/myProject/digital-twin-v2"


# ====== Post-Execute 事件自动写入框架 ======

def _after_tool_execute(tool_name: str, arguments: dict, result: str):
    """
    AI 执行工具后自动判断是否写入 Memory Event 或 Knowledge World。

    设计理念：
      来源 5 (执行结果自动采集) — 有价值的命令输出沉淀为长期 Knowledge。

    采集规则：
      - mysql -e "show create table" → Knowledge (DDL, 结构)
      - docker inspect                 → Knowledge (Config, 容器配置)
      - curl <API>                     → Knowledge (API response, 接口响应)
      - kubectl describe               → Knowledge (Resource detail, 资源详情)
      - git log/diff                   → Knowledge (Change history)

    :param tool_name: MCP 工具名（例如 "bash"、"edit"）
    :param arguments: 工具调用参数字典
    :param result: 工具结果字符串
    """

    # ---- 黑名单：无长期价值的临时查询命令 ----
    SKIP_TOOLS = {
        "ls", "cat", "echo", "cd", "pwd", "grep", "find", "head", "tail",
        "wc", "file", "which", "whoami", "date", "uname", "hostname",
        "dt_search_kg", "dt_search", "dt_health",
        "svc_status", "svc_logs", "svc_list",
        "kublog_status", "kublog_logs",
        "jcli_list", "jcli_params", "jcli_history", "jcli_build_log",
    }

    if tool_name in SKIP_TOOLS:
        return  # 纯查询/读操作，跳过

    # ---- 额外：bash 中调用的命令也检查 ----
    if tool_name == "bash":
        cmd_str = arguments.get("command", "")
        if not _is_valuable_command(cmd_str):
            return

    # ---- 判断结果结构化程度 ----
    if not _has_long_term_value(result):
        return

    # ---- 触发 dt memorize 沉淀 Knowledge ----
    _try_memorize_from_result(tool_name, arguments, result)


def _is_valuable_command(cmd_str: str) -> bool:
    """
    判断 bash 命令是否有长期价值。
    """
    VALUABLE_PREFIXES = [
        "mysql -e",
        "mysqldump",
        "docker inspect",
        "docker ps",
        "docker-compose",
        "curl ",
        "wget ",
        "kubectl describe",
        "kubectl get",
        "git log",
        "git diff",
        "git show",
        "git remote",
        "helm ",
        "nslookup",
        "dig ",
        "systemctl ",
        "pip ",
        "npm ",
        "apt ",
        "brew ",
    ]

    cmd_lower = cmd_str.lower().strip()
    for prefix in VALUABLE_PREFIXES:
        if cmd_lower.startswith(prefix.lower()):
            return True
    return False


def _has_long_term_value(result: str) -> bool:
    """
    判断输出是否有长期参考价值。

    规则:
      - 输出长度 > 100 字符（排除 "done" / "ok" 等）
      - 包含结构化标记：SQL DDL, JSON, YAML, 表格
      - 包含关键信息关键词
    """
    if not result or len(result.strip()) < 100:
        return False

    result_upper = result.upper()

    # 结构化标记
    structure_markers = [
        "CREATE TABLE", "ALTER TABLE", "DROP TABLE",
        '"', '{', '[',       # JSON-like
        "---",               # YAML-like
        "+----", "| ",       # MySQL table output
        "KIND:", "API VERSION:",  # kubectl YAML
        "READY", "STATUS",   # kubectl get output
        "COMMIT ",           # git log
        "DIFF ",             # git diff
        "AUTHOR:",           # git show
        "INSPECT",           # docker inspect
        "CONFIG",            # docker config
        "INSTALLED",         # pip/npm install
    ]

    for marker in structure_markers:
        if marker in result_upper:
            return True

    return False


def _try_memorize_from_result(tool_name: str, arguments: dict, result: str):
    """
    尝试将执行结果沉淀为 Knowledge。

    通过 subprocess 调用 `dt memorize` CLI。
    """
    # 生成 entity_id：基于工具名 + 参数唯一性
    cmd_or_args = arguments.get("command", "") or json.dumps(arguments, sort_keys=True)
    entity_id = f"exec-result/{tool_name}/{hash(cmd_or_args) & 0xFFFFFFFF:08x}"

    # 截断结果到合理长度
    truncated = result[:2000] if len(result) > 2000 else result

    details = (
        f"tool: {tool_name}; "
        f"args: {cmd_or_args[:120]}; "
        f"content: {truncated}"
    )

    try:
        subprocess.run(
            [
                DT_BIN, "memorize",
                "--type", "KnowledgeAdded",
                "--entity-id", entity_id,
                "--entity-type", "ExecutionResult",
                "--details", details,
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        logging.info("[post-execute] 已记忆: entity_id=%s", entity_id)
    except Exception as e:
        logging.warning("[post-execute] 记忆失败: %s", e)


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
    """执行命令并返回 stdout，清理 ANSI 转义序列
    """
    import shutil
    resolved = shutil.which(cmd[0]) or cmd[0]
    print(f"[CMD] {' '.join(cmd)}  (解析: {resolved})", file=sys.stderr, flush=True)
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, cwd=_DT_PROJECT_ROOT)
    output = (result.stdout + result.stderr).strip()
    # 清理 ANSI 颜色码
    output = re.sub(r'\x1b\[[0-9;]*m', '', output)
    return output or "(无输出)"


# ====== 工具注册 ======

@server.list_tools()
async def list_tools():
    return [
        # ===== 搜索 =====
        Tool(
            name="dt_search_kg",
            description="搜索知识图谱（GraphRAG 混合检索：向量召回+图扩展+rerank），返回 JSON（含 summary/来源文档/hop/score_breakdown）。world 可指定 code/knowledge/doc/config/memory/all，默认 knowledge（纯知识层）。推荐用法：①查询代码实体（类/方法）时推荐 world='code'；②若已知精确方法/类名（如 groupMsgRecall），直接把名字作为 query 会触发精确匹配（0.95 分置顶），比中文描述更准；③若已从 [DT-SENSE] 简报或上下文得知目标项目名，推荐同时带 project=<项目名> 过滤跨项目噪音。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "自然语言搜索关键词"},
                    "world": {"type": "string", "description": "检索世界: all|code|knowledge|doc|config|memory，默认 knowledge", "default": "knowledge"},
                    "project": {"type": "string", "description": "限定项目名（如 im-center），过滤跨项目噪音", "default": ""},
                    "limit": {"type": "integer", "description": "返回数量", "default": 10}
                }, "required": ["query"]
            }
        ),
        Tool(
            name="dt_search",
            description="统一检索（world: all|code|knowledge|doc|config|memory，默认 all），返回 JSON。Method 含 llm_analysis/file_path/start_line/end_line，Entity 含 summary/来源，Doc 含原文块。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "搜索关键词"},
                    "world": {"type": "string", "description": "搜索世界: all(代码+知识+文档) / code(代码方法) / knowledge(知识图谱) / doc(文档) / config(配置) / memory(事件)", "default": "all"},
                    "limit": {"type": "integer", "description": "返回数量", "default": 10},
                    "project": {"type": "string", "description": "限定项目名（可选）"}
                }, "required": ["query"]
            }
        ),
        Tool(
            name="dt_sense",
            description="环境感知（会话开始时的第一个动作）：定位目录所属项目，返回项目简报（统计/目录画像/语言/关键实体）；未注册目录返回候选项目发现报告。",
            inputSchema={
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "目标目录，缺省为当前工作目录"}
                }, "required": []
            }
        ),

        # ===== 知识 =====
        Tool(
            name="dt_memorize",
            description="写入知识节点到KG(架构决策、用户说'记住')。type 取值: Decision/KnowledgeAdded/Environment/Dependencies",
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
            description="写入事件节点到KG(部署/安装/配置变更/会话记录)。type 取值: Deploy/SoftwareInstalled/ConfigChange/Conversation",
            inputSchema={
                "type": "object",
                "properties": {
                    "type": {"type": "string", "description": "事件类型"},
                    "entity_id": {"type": "string", "description": "唯一标识"},
                    "entity_type": {"type": "string", "description": "实体类型"},
                    "project": {"type": "string", "description": "所属项目"},
                    "details": {"type": "string", "description": "详细内容"}
                }, "required": ["type", "entity_id", "entity_type", "details"]
            }
        ),
        Tool(
            name="dt_learn",
            description="从AI任务执行结果批量写入知识(模式/踩坑/决策)到Knowledge World。",
            inputSchema={
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "任务标题(e.g. 支付平台迁移)"},
                    "entities": {"type": "string", "description": "涉及实体，逗号分隔"},
                    "pattern": {"type": "string", "description": "解决方案模式"},
                    "pitfalls": {"type": "string", "description": "踩坑经验，逗号分隔"},
                    "decisions": {"type": "string", "description": "架构决策，逗号分隔"},
                    "thread_id": {"type": "string", "description": "Digital Thread ID"},
                    "success": {"type": "boolean", "description": "任务是否成功"},
                    "project": {"type": "string", "description": "所属项目"}
                }, "required": ["task"]
            }
        ),

        # ===== 管线 =====
        Tool(
            name="dt_build",
            description="增量构建：扫描项目，hash 对比文件，仅索引变更部分。path 支持项目目录或文件绝对路径，传文件时自动解析项目名。",
            inputSchema={
                "type": "object",
                "properties": {
                    "all": {"type": "boolean", "description": "构建 config.yaml 中所有项目", "default": False},
                    "full": {"type": "boolean", "description": "全量重建，绕过增量快照", "default": False},
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
        Tool(
            name="dt_kg_sync",
            description="同步KG节点到Qdrant向量库(KG→Qdrant桥接)。KG节点变更后应触发增量同步。等价于 dt build --source knowledge。",
            inputSchema={
                "type": "object",
                "properties": {
                    "config_chunks": {"type": "boolean", "description": "同时同步自适应配置分块到 config_chunks 集合", "default": False}
                }, "required": []
            }
        ),

        # ===== 服务: 本地微服务管理 =====
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

        # ===== K8s: K8s 日志与状态 =====
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

        # ===== Jenkins: Jenkins 部署 =====
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

        # ===== 运维 =====
        Tool(
            name="dt_health",
            description="检查所有后端服务健康状态(Memgraph/Embed/Qdrant/KG Bridge/Fulltext)",
            inputSchema={"type": "object", "properties": {}, "required": []}
        ),
        Tool(
            name="dt_backup",
            description="系统备份：分级备份 Memgraph/Qdrant/SQLite。支持 backup/restore/list/verify 四种操作。",
            inputSchema={
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "操作: backup/restore/list/verify",
                        "enum": ["backup", "restore", "list", "verify"],
                        "default": "backup"
                    },
                    "date": {"type": "string", "description": "恢复/校验的日期 YYYY-MM-DD"}
                }, "required": []
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

    # ===== 搜索 =====
    if name == "dt_search_kg":
        query = arguments.get("query", "")
        world = arguments.get("world", "knowledge")
        limit = arguments.get("limit", 10)
        project = arguments.get("project", "")
        cmd = [DT_BIN, "search", query, "--world", world,
               "--limit", str(limit), "--json"]
        if project:
            cmd += ["--project", project]
        text = run_cmd(cmd)

    elif name == "dt_search":
        query = arguments.get("query", "")
        world = arguments.get("world", "all")
        limit = arguments.get("limit", 10)
        project = arguments.get("project", "")
        cmd = [DT_BIN, "search", query, "--world", world,
               "--limit", str(limit), "--json"]
        if project:
            cmd += ["--project", project]
        text = run_cmd(cmd)

    elif name == "dt_sense":
        cmd = [DT_BIN, "sense"]
        if arguments.get("path"):
            cmd.append(arguments["path"])
        cmd += ["--json"]
        text = run_cmd(cmd, timeout=120)

    elif name == "dt_memorize":
        cmd = [DT_BIN, "memorize", "--type", arguments["type"], "--entity-id", arguments["entity_id"]]
        if arguments.get("entity_type"): cmd += ["--entity-type", arguments["entity_type"]]
        if arguments.get("project"): cmd += ["--project", arguments["project"]]
        cmd += ["--details", arguments.get("details", "")]
        text = run_cmd(cmd)

    elif name == "dt_event":
        cmd = [DT_BIN, "event", "--type", arguments["type"], "--entity-id", arguments["entity_id"]]
        if arguments.get("entity_type"): cmd += ["--entity-type", arguments["entity_type"]]
        if arguments.get("project"): cmd += ["--project", arguments["project"]]
        cmd += ["--details", arguments.get("details", "")]
        text = run_cmd(cmd)

    elif name == "dt_learn":
        task = arguments.get("task", "")
        cmd = [DT_BIN, "learn", "--task", task]
        if arguments.get("entities"):
            cmd += ["--entities", arguments["entities"]]
        if arguments.get("pattern"):
            cmd += ["--pattern", arguments["pattern"]]
        if arguments.get("pitfalls"):
            cmd += ["--pitfalls", arguments["pitfalls"]]
        if arguments.get("decisions"):
            cmd += ["--decisions", arguments["decisions"]]
        if arguments.get("thread_id"):
            cmd += ["--thread-id", arguments["thread_id"]]
        if arguments.get("success") is not None:
            cmd += ["--success", str(arguments["success"]).lower()]
        if arguments.get("project"):
            cmd += ["--project", arguments["project"]]
        text = run_cmd(cmd, timeout=120)

    elif name == "dt_build":
        if arguments.get("all"):
            cmd = [DT_BIN, "build", "--all"]
            if arguments.get("full"):
                cmd.append("--full")
            text = run_cmd(cmd, timeout=1800)
        else:
            paths = arguments.get("path", [])
            if isinstance(paths, str):
                paths = [paths]
            project_name = arguments.get("name", "")
            results = []
            for p in paths:
                if os.path.isfile(p):
                    r = run_cmd([DT_BIN, "build", "--file", p])
                else:
                    cmd = [DT_BIN, "build", "--path", p]
                    if project_name:
                        cmd += ["--name", project_name]
                    r = run_cmd(cmd, timeout=300)
                results.append(r)
            text = "\n".join(results)

    elif name == "dt_kg_sync":
        cmd = [DT_BIN, "build", "--source", "knowledge"]
        if arguments.get("config_chunks"):
            cmd.append("--config-chunks")
        text = run_cmd(cmd, timeout=300)

    # ===== 服务: 本地微服务管理 =====
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

    # ===== K8s: K8s 日志与状态 =====
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

    # ===== Jenkins: Jenkins 部署 =====
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

    # ===== 运维 =====
    elif name == "dt_health":
        text = run_cmd([DT_BIN, "health"])

    elif name == "dt_backup":
        action = arguments.get("action", "backup")
        cmd = [DT_BIN, "backup", "--action", action]
        if arguments.get("date"):
            cmd += ["--date", arguments["date"]]
        text = run_cmd(cmd, timeout=600)

    else:
        text = f"未知工具: {name}"

    ts_end = datetime.now()
    log_tool(name, arguments, ts_start, ts_end, text)

    # 执行后处理: 自动判定是否沉淀为 Knowledge/Memory Event
    _after_tool_execute(name, arguments, text)

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
