#!/home/luis/.local/miniconda3/bin/python3
"""
DT MCP Server V2 — 将 digital-twin CLI 命令注册为 OpenCode Tool

提供工具 (34个):
  搜索: dt_search_kg, dt_search_expand, dt_search
  分析: dt_context, dt_plan, dt_domain, dt_history, dt_dependency, dt_verify
  知识: dt_memorize, dt_event, dt_learn, dt_thread
  管线: dt_build, nacos_sync, dt_kg_sync
  服务: svc_list, svc_status, svc_logs, svc_start, svc_stop, svc_restart
  K8s:  kublog_status, kublog_logs, kublog_download
  Jenkins: jcli_list, jcli_params, jcli_history, jcli_build_log, jcli_build
  运维: dt_health, dt_cleanup, dt_backup, dt_metrics

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

    :param tool_name: MCP tool name (e.g. "bash", "edit")
    :param arguments: tool call arguments dict
    :param result: tool result string
    """

    # ---- 黑名单：无长期价值的临时查询命令 ----
    SKIP_TOOLS = {
        "ls", "cat", "echo", "cd", "pwd", "grep", "find", "head", "tail",
        "wc", "file", "which", "whoami", "date", "uname", "hostname",
        "dt_search_kg", "dt_search_expand", "dt_search", "dt_health",
        "svc_status", "svc_logs", "svc_list",
        "kublog_status", "kublog_logs",
        "jcli_list", "jcli_params", "jcli_history", "jcli_build_log",
        "dt_context", "dt_plan", "dt_domain", "dt_history", "dt_dependency",
        "dt_verify", "dt_metrics",
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
        logging.info("[post-execute] memorized: entity_id=%s", entity_id)
    except Exception as e:
        logging.warning("[post-execute] memorize failed: %s", e)


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
    print(f"[CMD] {' '.join(cmd)}  (resolved: {resolved})", file=sys.stderr, flush=True)
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
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
        Tool(
            name="dt_search",
            description="语义代码搜索(精简版，单查询)。通过 path 或 name 限定搜索范围，搜索代码世界。",
            inputSchema={
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "搜索关键词"},
                    "world": {"type": "string", "description": "搜索世界: code/knowledge/doc/all", "default": "code"},
                    "limit": {"type": "integer", "description": "返回数量", "default": 10},
                    "path": {"type": "string", "description": "搜索范围路径"}
                }, "required": ["query"]
            }
        ),

        # ===== 分析 =====
        Tool(
            name="dt_context",
            description="构建六世界聚合上下文。为任务聚合 code/knowledge/doc/domain/memory/playbook 六个世界的上下文。",
            inputSchema={
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "任务描述"},
                    "worlds": {"type": "string", "description": "查询的世界(逗号分隔，如 code,knowledge)"},
                    "max_tokens": {"type": "integer", "description": "最大 token 数"},
                    "thread_id": {"type": "string", "description": "Digital Thread ID"}
                }, "required": ["task"]
            }
        ),
        Tool(
            name="dt_plan",
            description="为任务生成执行计划，基于 Playbook 匹配。可接收 dt_context 输出作为上下文。",
            inputSchema={
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "任务描述"},
                    "context": {"type": "string", "description": "来自 dt_context 的上下文(可选)"},
                    "thread_id": {"type": "string", "description": "Digital Thread ID"}
                }, "required": ["task"]
            }
        ),
        Tool(
            name="dt_domain",
            description="查询领域知识模型子图。按领域名深度遍历相关实体。",
            inputSchema={
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "领域名称 (e.g. \"支付\", \"部署\")"},
                    "depth": {"type": "integer", "description": "遍历深度", "default": 2},
                    "include_code": {"type": "boolean", "description": "是否包含代码实体", "default": False}
                }, "required": ["name"]
            }
        ),
        Tool(
            name="dt_history",
            description="从记忆世界检索相似历史任务。用于回溯经验、参考过往方案。",
            inputSchema={
                "type": "object",
                "properties": {
                    "task": {"type": "string", "description": "任务描述(用于相似匹配)"},
                    "domain": {"type": "string", "description": "领域过滤器"},
                    "days": {"type": "integer", "description": "回溯天数", "default": 90},
                    "limit": {"type": "integer", "description": "最大结果数", "default": 5}
                }, "required": ["task"]
            }
        ),
        Tool(
            name="dt_dependency",
            description="分析调用链和依赖影响。分析指定实体的上下游依赖关系。",
            inputSchema={
                "type": "object",
                "properties": {
                    "target": {"type": "string", "description": "目标实体(方法名/类名/服务名)"},
                    "direction": {
                        "type": "string",
                        "description": "方向: upstream/downstream/both",
                        "enum": ["upstream", "downstream", "both"],
                        "default": "both"
                    },
                    "depth": {"type": "integer", "description": "遍历深度", "default": 2},
                    "type": {
                        "type": "string",
                        "description": "依赖类型: code/config/service/all",
                        "default": "all"
                    }
                }, "required": ["target"]
            }
        ),
        Tool(
            name="dt_verify",
            description="代码变更后的一致性校验。支持配置、数据库、API 签名检查。",
            inputSchema={
                "type": "object",
                "properties": {
                    "files": {"type": "string", "description": "变更文件路径(逗号分隔)"},
                    "check_config": {"type": "boolean", "description": "检查 Nacos 配置一致性", "default": False},
                    "check_db": {"type": "boolean", "description": "检查数据库 schema 一致性", "default": False},
                    "check_api": {"type": "boolean", "description": "检查 API 签名一致性", "default": False}
                }, "required": []
            }
        ),

        # ===== 知识 =====
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
        Tool(
            name="dt_thread",
            description="管理 Digital Thread 生命周期：创建、追加会话、追加决策、查看、关闭。",
            inputSchema={
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "操作: create/add-session/add-decision/get/list/close",
                        "enum": ["create", "add-session", "add-decision", "get", "list", "close"],
                        "default": "list"
                    },
                    "name": {"type": "string", "description": "Thread 名称(create 时使用)"},
                    "description": {"type": "string", "description": "Thread 描述(create 时使用)"},
                    "thread_id": {"type": "string", "description": "Thread ID(add-session/add-decision/get/close 时使用)"},
                    "session_id": {"type": "string", "description": "Session ID(add-session 时使用)"},
                    "decision_id": {"type": "string", "description": "Decision ID(add-decision 时使用)"}
                }, "required": []
            }
        ),

        # ===== 管线 =====
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
            description="检查所有后端服务健康状态(Neo4j/Embed/Qdrant/KG Bridge/Fulltext)",
            inputSchema={"type": "object", "properties": {}, "required": []}
        ),
        Tool(
            name="dt_cleanup",
            description="分级清理：预览或执行清理 reasoning/memory/snapshots。默认 dry-run 预览模式。",
            inputSchema={
                "type": "object",
                "properties": {
                    "dry_run": {"type": "boolean", "description": "预览模式(仅显示，不执行)", "default": True},
                    "execute": {"type": "boolean", "description": "执行清理", "default": False},
                    "targets": {
                        "type": "string",
                        "description": "清理目标(逗号分隔): reasoning/memory/snapshots/all",
                        "default": "all"
                    }
                }, "required": []
            }
        ),
        Tool(
            name="dt_backup",
            description="系统备份：分级备份 Neo4j/Qdrant/SQLite。支持 backup/restore/list/verify 四种操作。",
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
        Tool(
            name="dt_metrics",
            description="查询系统 metrics(通过 gRPC)。支持 watch 模式和按名称过滤。",
            inputSchema={
                "type": "object",
                "properties": {
                    "watch": {"type": "boolean", "description": "持续 watch 模式", "default": False},
                    "interval": {"type": "integer", "description": "轮询间隔(秒)", "default": 5},
                    "filter": {"type": "string", "description": "按名称过滤(glob, 如 dt_build*)"}
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
        limit = arguments.get("limit", 10)
        text = run_cmd([DT_BIN, "search-kg", query, "--limit", str(limit)])

    elif name == "dt_search_expand":
        query = arguments.get("query", "")
        path = arguments.get("path", "")
        project = arguments.get("name", "")
        limit = arguments.get("limit", 10)
        cmd = [DT_BIN, "search", "--query", query, "--world", "code", "--limit", str(limit)]
        if path:
            cmd += ["--path", path]
        elif project:
            cmd += ["--path", project]
        text = run_cmd(cmd)

    elif name == "dt_search":
        query = arguments.get("query", "")
        world = arguments.get("world", "code")
        limit = arguments.get("limit", 10)
        path = arguments.get("path", "")
        cmd = [DT_BIN, "search", "--query", query, "--world", world, "--limit", str(limit)]
        if path:
            cmd += ["--path", path]
        text = run_cmd(cmd)

    # ===== 分析 =====
    elif name == "dt_context":
        task = arguments.get("task", "")
        cmd = [DT_BIN, "context", "--task", task]
        if arguments.get("worlds"):
            cmd += ["--worlds", arguments["worlds"]]
        if arguments.get("max_tokens"):
            cmd += ["--max-tokens", str(arguments["max_tokens"])]
        if arguments.get("thread_id"):
            cmd += ["--thread-id", arguments["thread_id"]]
        text = run_cmd(cmd, timeout=300)

    elif name == "dt_plan":
        task = arguments.get("task", "")
        cmd = [DT_BIN, "plan", "--task", task]
        if arguments.get("context"):
            cmd += ["--context", arguments["context"]]
        if arguments.get("thread_id"):
            cmd += ["--thread-id", arguments["thread_id"]]
        text = run_cmd(cmd, timeout=300)

    elif name == "dt_domain":
        name_val = arguments.get("name", "")
        cmd = [DT_BIN, "domain", "--name", name_val]
        if arguments.get("depth"):
            cmd += ["--depth", str(arguments["depth"])]
        if arguments.get("include_code"):
            cmd += ["--include-code"]
        text = run_cmd(cmd, timeout=120)

    elif name == "dt_history":
        task = arguments.get("task", "")
        cmd = [DT_BIN, "history", "--task", task]
        if arguments.get("domain"):
            cmd += ["--domain", arguments["domain"]]
        if arguments.get("days"):
            cmd += ["--days", str(arguments["days"])]
        if arguments.get("limit"):
            cmd += ["--limit", str(arguments["limit"])]
        text = run_cmd(cmd, timeout=120)

    elif name == "dt_dependency":
        target = arguments.get("target", "")
        cmd = [DT_BIN, "dependency", "--target", target]
        if arguments.get("direction"):
            cmd += ["--direction", arguments["direction"]]
        if arguments.get("depth"):
            cmd += ["--depth", str(arguments["depth"])]
        if arguments.get("type"):
            cmd += ["--type", arguments["type"]]
        text = run_cmd(cmd, timeout=120)

    elif name == "dt_verify":
        cmd = [DT_BIN, "verify"]
        if arguments.get("files"):
            cmd += ["--files", arguments["files"]]
        if arguments.get("check_config"):
            cmd += ["--check-config"]
        if arguments.get("check_db"):
            cmd += ["--check-db"]
        if arguments.get("check_api"):
            cmd += ["--check-api"]
        text = run_cmd(cmd, timeout=120)

    # ===== 知识 =====
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

    elif name == "dt_thread":
        action = arguments.get("action", "list")
        cmd = [DT_BIN, "thread", "--action", action]
        if arguments.get("name"):
            cmd += ["--name", arguments["name"]]
        if arguments.get("description"):
            cmd += ["--description", arguments["description"]]
        if arguments.get("thread_id"):
            cmd += ["--thread-id", arguments["thread_id"]]
        if arguments.get("session_id"):
            cmd += ["--session-id", arguments["session_id"]]
        if arguments.get("decision_id"):
            cmd += ["--decision-id", arguments["decision_id"]]
        text = run_cmd(cmd, timeout=120)

    # ===== 管线 =====
    elif name == "dt_build":
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

    elif name == "nacos_sync":
        env = arguments.get("env", "all")
        text = run_cmd([DT_BIN, "nacos-sync", "--env", env], timeout=300)

    elif name == "dt_kg_sync":
        cmd = [DT_BIN, "kg-sync"]
        if arguments.get("incremental"):
            cmd.append("--incremental")
        if arguments.get("labels"):
            for lbl in arguments["labels"].split(","):
                cmd += ["--labels", lbl.strip()]
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

    elif name == "dt_cleanup":
        cmd = [DT_BIN, "cleanup"]
        dry_run = arguments.get("dry_run", True)
        execute = arguments.get("execute", False)
        if execute:
            cmd += ["--execute"]
        elif dry_run:
            cmd += ["--dry-run"]
        targets = arguments.get("targets", "all")
        cmd += ["--targets", targets]
        text = run_cmd(cmd, timeout=300)

    elif name == "dt_backup":
        action = arguments.get("action", "backup")
        cmd = [DT_BIN, "backup", "--action", action]
        if arguments.get("date"):
            cmd += ["--date", arguments["date"]]
        text = run_cmd(cmd, timeout=600)

    elif name == "dt_metrics":
        cmd = [DT_BIN, "metrics"]
        if arguments.get("watch"):
            cmd += ["--watch"]
        if arguments.get("interval"):
            cmd += ["--interval", str(arguments["interval"])]
        if arguments.get("filter"):
            cmd += ["--filter", arguments["filter"]]
        text = run_cmd(cmd, timeout=300)

    else:
        text = f"未知工具: {name}"

    ts_end = datetime.now()
    log_tool(name, arguments, ts_start, ts_end, text)

    # Post-execute: 自动判定是否沉淀为 Knowledge/Memory Event
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
