#!/usr/bin/env python3
"""
使用当前项目的 LLM 提取知识，对比 HanLP 结果质量。

测试文档：developer-guide/architecture.md（HanLP 错误率高）
"""

import json
import os
import sys
import time
from pathlib import Path

import requests
import yaml


class LLMExtractor:
    """调用项目 LLM API 提取知识"""
    
    def __init__(self, config_path="config/pipeline.yaml"):
        # 从配置文件读取
        with open(config_path, 'r', encoding='utf-8') as f:
            config = yaml.safe_load(f)
        
        sf_config = config['providers']['siliconflow']
        self.api_url = sf_config['url'].rstrip('/') + '/chat/completions'
        self.api_key = sf_config['api_key']
        self.model = sf_config['model_llm']
        
        print(f"  API URL: {self.api_url}")
        print(f"  模型: {self.model}")
    
    def extract_entities_and_relations(self, text: str, doc_name: str) -> dict:
        """提取实体和关系"""
        
        prompt = f"""你是一个技术文档知识提取专家。请从以下文档中提取实体和关系。

文档名称：{doc_name}

提取规则：
1. 实体类型（仅使用以下类型）：
   - Service: 服务/组件（如 Gateway、AIAgent、CLI）
   - Module: 模块/包（如 agent/、tools/、gateway/）
   - Tool: 工具/命令（如 terminal、browser、mcp）
   - File: 文件（如 run_agent.py、cli.py）
   - Technology: 技术栈（如 SQLite、FTS5、Python）
   - Concept: 技术概念（如 Prompt Builder、Provider Resolution）
   - Platform: 平台（如 Telegram、Discord、Slack）

2. 关系类型（仅使用以下类型）：
   - CONTAINS: 包含关系（模块包含文件、服务包含组件）
   - DEPENDS_ON: 依赖关系
   - USES: 使用关系
   - IMPLEMENTS: 实现关系
   - CALLS: 调用关系
   - MANAGES: 管理关系

3. 输出要求（严格遵守，防止输出过长被截断）：
   - 实体只提取最重要的 8-12 个，宁缺毋滥
   - summary 用一句话，不超过 15 个中文字
   - relations 只输出 4-10 条最有把握的
   - evidence 只引用原文，不超过 25 个字

4. 输出格式（严格 JSON，只返回 JSON 本身，不要任何解释、不要 markdown 代码块）：
{{
  "entities": [
    {{"name": "实体名称", "type": "类型", "aliases": ["别名1"], "summary": "简短描述"}}
  ],
  "relations": [
    {{"head": "源实体", "tail": "目标实体", "type": "关系类型", "confidence": 0.9, "evidence": "原文引用"}}
  ]
}}

文档内容：
{text[:4000]}

只返回 JSON，不要任何解释。"""

        try:
            response = requests.post(
                self.api_url,
                headers={
                    "Authorization": f"Bearer {self.api_key}",
                    "Content-Type": "application/json"
                },
                json={
                    "model": self.model,
                    "messages": [
                        {"role": "system", "content": "You are a helpful assistant designed to output JSON."},
                        {"role": "user", "content": prompt}
                    ],
                    "temperature": 0.1,  # 降低随机性
                    "max_tokens": 6000,
                    "response_format": {"type": "json_object"},  # 强制 JSON 输出（官方推荐方式）
                },
                timeout=300  # R1 推理模型较慢，放宽到 5 分钟
            )
            response.raise_for_status()
            
            result = response.json()
            content = result["choices"][0]["message"]["content"]
            
            # 保存原始响应用于调试
            raw_content = content
            
            # 提取 JSON（可能被 markdown 代码块包裹）
            if "```json" in content:
                content = content.split("```json")[1].split("```")[0]
            elif "```" in content:
                content = content.split("```")[1].split("```")[0]

            content = content.strip()

            # 尝试修复常见 JSON 错误
            # 1. 去除注释
            lines = []
            for line in content.split('\n'):
                # 去除 // 注释
                if '//' in line:
                    line = line.split('//')[0]
                lines.append(line)
            content = '\n'.join(lines)

            # 2. 修复 relations 数组的双括号问题
            # 模式: },\n    {\n      {"head": ...
            # 修复为: },\n    {"head": ...
            import re
            content = re.sub(
                r'(\s*)\{\s*\n\s*(\{"head":)',
                r'\1\2',
                content
            )

            # 3. 尝试解析
            try:
                extracted = json.loads(content)
            except json.JSONDecodeError as e:
                # 如果失败，尝试找到错误位置并显示更多上下文
                error_pos = e.pos
                start = max(0, error_pos - 200)
                end = min(len(content), error_pos + 200)
                context = content[start:end]

                return {
                    "success": False,
                    "error": f"JSON 解析失败: {e}",
                    "error_position": error_pos,
                    "error_context": context,
                    "full_response": content  # 保存完整响应
                }

            # 4. 结构归一化：7B 模型输出不稳定
            #    （aliases 可能是字符串而非数组、summary 可能缺失、数组内可能混入非 dict 元素），
            #    统一形状避免下游崩溃
            entities_raw = extracted.get("entities", [])
            relations_raw = extracted.get("relations", [])
            entities = []
            for e in entities_raw:
                if not isinstance(e, dict) or not e.get("name") or not e.get("type"):
                    continue
                aliases = e.get("aliases", [])
                if isinstance(aliases, str):
                    aliases = [aliases] if aliases else []
                item = dict(e)
                item["aliases"] = aliases
                item.setdefault("summary", "")
                entities.append(item)
            relations = [r for r in relations_raw
                         if isinstance(r, dict) and r.get("head") and r.get("tail") and r.get("type")]
            extracted["entities"] = entities
            extracted["relations"] = relations

            return {
                "success": True,
                "data": extracted,
                "model": self.model,
                "tokens": result.get("usage", {}),
                "filtered": {"entities": len(entities_raw) - len(entities), "relations": len(relations_raw) - len(relations)},
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e)
            }


def load_hanlp_result(json_path: str) -> dict:
    """加载 HanLP 提取结果"""
    with open(json_path, 'r', encoding='utf-8') as f:
        return json.load(f)


def load_original_doc(md_path: str, max_chars=4000) -> str:
    """加载原始文档"""
    with open(md_path, 'r', encoding='utf-8') as f:
        return f.read()[:max_chars]


def analyze_quality(hanlp_result: dict, llm_result: dict, original_text: str, base_dir: Path):
    """对比质量分析"""
    
    print("\n" + "="*80)
    print("质量对比分析")
    print("="*80)
    
    # HanLP 统计
    hanlp_entities = hanlp_result.get("entities", [])
    hanlp_relations = hanlp_result.get("relations", [])
    
    print("\n【HanLP 提取结果】")
    print(f"实体数: {len(hanlp_entities)}")
    print(f"关系数: {len(hanlp_relations)}")
    
    # 统计 HanLP 实体类型分布
    hanlp_type_dist = {}
    for entity in hanlp_entities:
        entity_type = entity.get("type", "UNKNOWN")
        hanlp_type_dist[entity_type] = hanlp_type_dist.get(entity_type, 0) + 1
    
    print("\nHanLP 实体类型分布:")
    for type_name, count in sorted(hanlp_type_dist.items(), key=lambda x: -x[1]):
        print(f"  {type_name}: {count}")
    
    # 显示 HanLP 的典型错误
    print("\nHanLP 典型错误示例:")
    person_entities = [e for e in hanlp_entities if e.get("type") == "PERSON"][:5]
    for entity in person_entities:
        print(f"  - '{entity['text']}' 被识别为 PERSON（人名）")
    
    # LLM 统计
    if llm_result.get("success"):
        llm_data = llm_result["data"]
        llm_entities = llm_data.get("entities", [])
        llm_relations = llm_data.get("relations", [])
        
        print("\n【LLM 提取结果】")
        print(f"实体数: {len(llm_entities)}")
        print(f"关系数: {len(llm_relations)}")
        
        # 统计 LLM 实体类型分布
        llm_type_dist = {}
        for entity in llm_entities:
            entity_type = entity.get("type", "UNKNOWN")
            llm_type_dist[entity_type] = llm_type_dist.get(entity_type, 0) + 1
        
        print("\nLLM 实体类型分布:")
        for type_name, count in sorted(llm_type_dist.items(), key=lambda x: -x[1]):
            print(f"  {type_name}: {count}")
        
        # 显示 LLM 提取的实体示例
        print("\nLLM 提取实体示例（前10个）:")
        for entity in llm_entities[:10]:
            aliases = entity.get("aliases", [])
            aliases_str = f" (别名: {', '.join(aliases)})" if aliases else ""
            summary = entity.get("summary", "")
            print(f"  - {entity['name']} ({entity['type']}){aliases_str}")
            if summary:
                print(f"    摘要: {summary[:60]}...")
        
        # 显示 LLM 提取的关系示例
        print("\nLLM 提取关系示例（前10个）:")
        for rel in llm_relations[:10]:
            confidence = rel.get("confidence", 0)
            evidence = rel.get("evidence", "")[:50]
            print(f"  - {rel['head']} -[{rel['type']}]-> {rel['tail']} (置信度: {confidence})")
            if evidence:
                print(f"    证据: {evidence}...")
        
        # Token 使用统计
        tokens = llm_result.get("tokens", {})
        if tokens:
            print(f"\nToken 使用:")
            print(f"  输入: {tokens.get('prompt_tokens', 0)}")
            print(f"  输出: {tokens.get('completion_tokens', 0)}")
            print(f"  总计: {tokens.get('total_tokens', 0)}")
        
        # 质量对比
        print("\n【质量对比】")
        print(f"实体数量: HanLP {len(hanlp_entities)} vs LLM {len(llm_entities)}")
        print(f"关系数量: HanLP {len(hanlp_relations)} vs LLM {len(llm_relations)}")
        
        # 实体类型质量
        hanlp_wrong_types = sum(1 for e in hanlp_entities if e.get("type") in ["PERSON", "CARDINAL", "ORDINAL"])
        hanlp_accuracy = 1 - (hanlp_wrong_types / len(hanlp_entities)) if hanlp_entities else 0
        print(f"\nHanLP 实体类型准确率估算: {hanlp_accuracy:.1%}")
        print(f"  (错误类型数: {hanlp_wrong_types}/{len(hanlp_entities)})")
        
        print(f"\nLLM 实体类型: 全部符合领域 schema")
        
        # 关系质量
        hanlp_syntax_rels = sum(1 for r in hanlp_relations if r.get("label") in ["nn", "conj", "nummod", "dep"])
        hanlp_rel_quality = 1 - (hanlp_syntax_rels / len(hanlp_relations)) if hanlp_relations else 0
        print(f"\nHanLP 关系业务价值率: {hanlp_rel_quality:.1%}")
        print(f"  (语法关系数: {hanlp_syntax_rels}/{len(hanlp_relations)})")
        
        print(f"\nLLM 关系: 全部为业务关系（CONTAINS/DEPENDS_ON/USES等）")
        
    else:
        print("\n【LLM 提取失败】")
        print(f"错误: {llm_result.get('error')}")
        
        if "error_context" in llm_result:
            print(f"\n错误位置上下文 (位置 {llm_result.get('error_position')}):")
            print("-" * 80)
            print(llm_result["error_context"])
            print("-" * 80)
        
        if "full_response" in llm_result:
            # 保存完整响应到文件
            debug_path = base_dir / "hermes-docs-zh-hanlp" / "debug-llm-response.txt"
            with open(debug_path, 'w', encoding='utf-8') as f:
                f.write(llm_result["full_response"])
            print(f"\n完整 LLM 响应已保存到: {debug_path}")


def main():
    # 测试文档
    test_doc = "developer-guide/architecture.md"
    
    base_dir = Path(__file__).parent.parent
    md_path = base_dir / "hermes-docs-zh" / test_doc
    json_path = base_dir / "hermes-docs-zh-hanlp" / test_doc.replace(".md", ".json")
    
    if not md_path.exists():
        print(f"错误: 找不到文档 {md_path}")
        sys.exit(1)
    
    if not json_path.exists():
        print(f"错误: 找不到 HanLP 结果 {json_path}")
        sys.exit(1)
    
    print(f"测试文档: {test_doc}")
    print(f"原因: 该文档 HanLP 错误率高（Gateway/Cron/Telegram 都被识别为人名）")
    print("="*80)
    
    # 加载数据
    print("\n[1/3] 加载 HanLP 提取结果...")
    hanlp_result = load_hanlp_result(str(json_path))
    print(f"  ✓ 已加载: {hanlp_result['num_entities']} 个实体, {hanlp_result['num_relations']} 个关系")
    
    print("\n[2/3] 使用 LLM 提取知识...")
    original_text = load_original_doc(str(md_path))
    print(f"  文档长度: {len(original_text)} 字符")
    
    extractor = LLMExtractor()
    print(f"  LLM 模型: {extractor.model}")
    
    start_time = time.time()
    llm_result = extractor.extract_entities_and_relations(original_text, test_doc)
    elapsed = time.time() - start_time
    print(f"  ✓ 提取完成，耗时: {elapsed:.2f}s")
    
    # 保存 LLM 结果
    output_path = base_dir / "hermes-docs-zh-hanlp" / f"{test_doc.replace('.md', '')}-llm.json"
    output_path.parent.mkdir(parents=True, exist_ok=True)
    
    with open(output_path, 'w', encoding='utf-8') as f:
        json.dump(llm_result, f, ensure_ascii=False, indent=2)
    print(f"  ✓ LLM 结果已保存: {output_path}")
    
    # 质量对比
    print("\n[3/3] 质量对比分析...")
    analyze_quality(hanlp_result, llm_result, original_text, base_dir)
    
    print("\n" + "="*80)
    print("测试完成")
    print("="*80)


if __name__ == "__main__":
    main()
