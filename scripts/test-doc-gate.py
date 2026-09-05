#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""doc-gate prompt 真实 LLM 验证：三种文档各一发，确认分级 JSON 可解析。
只读 pipeline.yaml 的 url/model/api_key，不打印 key。"""
import json
import sys
import yaml
import requests

sys.path.insert(0, '/data/myProject/digital-twin-v2/scripts')
cfg = yaml.safe_load(open('/data/myProject/digital-twin-v2/config/pipeline.yaml'))
llm = cfg['llm']
prov = cfg['providers']['siliconflow']
url = prov['url'].rstrip('/') + '/chat/completions'
model = llm['model']
api_key = prov['api_key']

SYSTEM = """你是文档价值分级专家。判断给定文档是否值得进行
详细的实体/关系知识图谱提取。仅输出 JSON。

输出格式（严格 JSON，不要 markdown 围栏，不要额外说明）：
{"value": "high|medium|low", "reason": "一句话理由"}

分级标准：
- "high"：架构设计、技术方案、接口契约、规范/标准/SOP、配置说明、
  协议定义等——实体与关系有长期复用价值，值得详细提取（实体+关系）。
- "medium"：README、使用说明、操作手册、团队约定、会议纪要等——
  有一定知识密度，但关系价值有限，只提取实体+摘要即可。
- "low"：变更记录、流水账、日志、临时笔记、草稿、目录索引、模板
  填充说明等——知识密度低，不值得 LLM 详细提取，仅保留原文检索。

判断依据：文档类型、信息密度、实体/关系的可复用性。"""

DOCS = [
    ("架构设计.md", "high", """# 支付网关架构设计
## 总体架构
支付网关采用分层架构：接入层(API Gateway) → 路由层 → 核心引擎 → 渠道适配层。
## 核心组件
- PaymentRouter: 负责将交易路由到对应渠道，支持按金额/商户/渠道权重路由。
- ChannelAdapter: 渠道适配器接口，银联/支付宝/微信分别实现该接口。
- TransactionEngine: 交易状态机，管理 创建→支付中→成功/失败 状态流转。
## 依赖关系
PaymentRouter depends_on TransactionEngine；ChannelAdapter 由各渠道实现，
银联适配器 ChannelAdapterImpl 依赖银联 SDK 的 UnionPayClient。
## 接口契约
POST /api/v1/payments 创建支付；GET /api/v1/payments/{id} 查询状态。
超时时间 30s，幂等键 requestId 必填。"""),
    ("README.md", "medium", """# 支付网关项目

支付网关是公司统一的支付接入平台，支持多种渠道。

## 快速开始

git clone 后执行 mvn install，然后启动 application。

## 目录结构

- src/main/java 业务代码
- src/test 单元测试
- docs 文档目录

## 常用命令

mvn clean package 打包；mvn test 跑测试。

## 联系方式

有问题联系支付组 @zhangsan。"""),
    ("变更记录.md", "low", """# 变更记录 2026-09-05

## 15:30 修改 PaymentRouter.java
- 修复了路由超时设置错误的问题
- 改了超时从 30s 到 60s
（reviewer: lisi）

## 16:00 数据库脚本
- 执行了 alter table payment add column channel_code
- 回滚脚本见同目录 rollback.sql

## 16:30 部署
- 灰度发布 v1.2.3 到 5% 流量
- 观察 30 分钟无异常后全量

## 待办
- 明天确认下渠道对账问题"""),
]

def call(name, text):
    r = requests.post(
        url,
        headers={"Authorization": f"Bearer {api_key}"},
        json={
            "model": model,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": f"文件：{name}\n\n文档内容：\n{text[:2000]}"},
            ],
            "temperature": 0.0,
            "max_tokens": 80,
        },
        timeout=60,
    )
    r.raise_for_status()
    return r.json()["choices"][0]["message"]["content"].strip()

ok = 0
for name, expect, text in DOCS:
    try:
        raw = call(name, text)
        # 提取 JSON
        s, e = raw.find("{"), raw.rfind("}")
        parsed = json.loads(raw[s:e+1]) if s >= 0 and e > s else {}
        val = parsed.get("value")
        good = val == expect
        ok += good
        print(f"[{'PASS' if good else 'FAIL'}] {name}: expect={expect} got={val} reason={parsed.get('reason','')[:40]}")
        if not good:
            print(f"      raw={raw[:120]!r}")
    except Exception as ex:
        print(f"[ERROR] {name}: {ex}")
print(f"\n{ok}/3 正确")
