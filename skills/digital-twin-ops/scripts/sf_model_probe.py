#!/usr/bin/env python3
"""硅基流动模型对 dt 管线的适配性探测。

用途: 换 pipeline.yaml 的 model_llm 前, 模拟 dt 的完整 LLM 请求(真实 nacos_config
prompt + 样例 yaml + max_tokens 4096), 确认响应 content 非空且是纯 JSON ——
推理模型(Qwen/Qwen3.5-9B 等)输出全在 reasoning_content, content 恒空,
dt 的 llm_client 解析空串报 "EOF while parsing a value", 数据零入库。

用法: python3 sf_model_probe.py [模型名...]
默认: Qwen/Qwen3-14B Qwen/Qwen3.5-9B deepseek-ai/DeepSeek-V3.2
"""
import json
import re
import sys
import urllib.request
import urllib.error

PIPELINE = "/home/luis/.config/digital-twin/pipeline.yaml"
PROMPT_FILE = "/data/myProject/digital-twin-v2/config/prompts/nacos_config.yaml"

SAMPLE_YAML = (
    "spring:\n"
    "  datasource:\n"
    "    url: jdbc:mysql://10.0.0.5:3306/uvp_pay?useSSL=false\n"
    "    username: root\n"
    "    password: 123456\n"
    "server:\n"
    "  port: 8080\n"
)


def load_key() -> str:
    with open(PIPELINE, encoding="utf-8") as f:
        c = f.read()
    m = re.search(r'api_key:\s*["\x27]?([^"\x27\n]*)', c)
    if not m:
        sys.exit("ERROR: pipeline.yaml 未找到 api_key")
    return m.group(1).strip()


def load_prompt() -> tuple[str, str]:
    with open(PROMPT_FILE, encoding="utf-8") as f:
        content = f.read()
    sys_m = re.search(r"system: \|(.*?)(?=\n\w+: |\Z)", content, re.S)
    prompt_m = re.search(r"prompt: \|(.*?)(?=\n\w+: |\Z)", content, re.S)
    return (
        sys_m.group(1).strip() if sys_m else "",
        prompt_m.group(1).strip() if prompt_m else "",
    )


def probe(key: str, system: str, prompt_tpl: str, model: str) -> None:
    user = (
        prompt_tpl.replace("${file_path}", "dt://nacos/test/DEFAULT_GROUP/uvp-pay.yaml")
        .replace("${file_text}", SAMPLE_YAML)
    )
    body = {
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "temperature": 0.1,
        "max_tokens": 4096,
        "stream": False,
    }
    req = urllib.request.Request(
        "https://api.siliconflow.cn/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Authorization": "Bearer " + key, "Content-Type": "application/json"},
        method="POST",
    )
    print("===", model, "===")
    try:
        resp = urllib.request.urlopen(req, timeout=180)
        d = json.loads(resp.read())
        msg = d["choices"][0]["message"]
        content = msg.get("content", "")
        reasoning = msg.get("reasoning_content", "")
        print("  finish_reason:", d["choices"][0].get("finish_reason"))
        print("  content长度:", len(content), "| reasoning长度:", len(reasoning))
        print("  content前120字:", repr(content[:120]))
        ok = len(content.strip()) > 0
        print("  → dt 管线可用:", "✅" if ok else "❌ (推理模型, 勿用)")
    except urllib.error.HTTPError as e:
        print("  HTTP", e.code, e.read()[:200])
    except Exception as e:
        print("  错误:", e)


if __name__ == "__main__":
    models = sys.argv[1:] or [
        "Qwen/Qwen3-14B",
        "Qwen/Qwen3.5-9B",
        "deepseek-ai/DeepSeek-V3.2",
    ]
    key = load_key()
    system, prompt_tpl = load_prompt()
    for m in models:
        probe(key, system, prompt_tpl, m)
