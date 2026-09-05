#!/usr/bin/env python3
"""
测试 LLM 提取的稳定性：运行 10 次，统计成功率和结果一致性
"""

import json
import sys
import time
from pathlib import Path

import yaml
import requests


class LLMExtractor:
    """调用项目 LLM API 提取知识"""
    
    def __init__(self, config_path="config/pipeline.yaml", model_override=None):
        with open(config_path, 'r', encoding='utf-8') as f:
            config = yaml.safe_load(f)
        
        sf_config = config['providers']['siliconflow']
        self.api_url = sf_config['url'].rstrip('/') + '/chat/completions'
        self.api_key = sf_config['api_key']
        # 允许覆盖模型
        self.model = model_override or sf_config['model_llm']
    
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
   - CONTAINS: 包含关系
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
                    "temperature": 0.1,
                    "max_tokens": 6000,
                    "response_format": {"type": "json_object"},  # 强制 JSON 输出（官方推荐方式）
                },
                timeout=300  # R1 推理模型较慢，放宽到 5 分钟
            )
            response.raise_for_status()
            
            result = response.json()
            content = result["choices"][0]["message"]["content"]
            
            # 提取 JSON
            if "```json" in content:
                content = content.split("```json")[1].split("```")[0]
            elif "```" in content:
                content = content.split("```")[1].split("```")[0]
            
            content = content.strip()
            
            # 去除注释
            lines = []
            for line in content.split('\n'):
                if '//' in line:
                    line = line.split('//')[0]
                lines.append(line)
            content = '\n'.join(lines)
            
            # 修复双括号问题
            import re
            content = re.sub(r'(\s*)\{\s*\n\s*(\{"head":)', r'\1\2', content)

            extracted = json.loads(content)

            # 结构归一化：7B 模型输出不稳定
            # （aliases 可能是字符串而非数组、summary 可能缺失、数组内可能混入非 dict 元素），
            # 统一形状避免下游崩溃
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
                "tokens": result.get("usage", {}),
                "filtered": {"entities": len(entities_raw) - len(entities), "relations": len(relations_raw) - len(relations)},
            }
            
        except Exception as e:
            return {
                "success": False,
                "error": str(e)
            }


def main():
    import sys
    
    # 从命令行参数读取模型（可选）
    model_override = sys.argv[1] if len(sys.argv) > 1 else None
    
    base_dir = Path(__file__).parent.parent
    test_doc = "developer-guide/architecture.md"
    md_path = base_dir / "hermes-docs-zh" / test_doc
    
    if not md_path.exists():
        print(f"错误: 找不到文档 {md_path}")
        sys.exit(1)
    
    with open(md_path, 'r', encoding='utf-8') as f:
        text = f.read()[:4000]
    
    print("="*80)
    print("LLM 提取稳定性测试")
    print("="*80)
    print(f"测试文档: {test_doc}")
    print(f"文档长度: {len(text)} 字符")
    print(f"测试次数: 10 次")
    print(f"Temperature: 0.1 (低随机性)")
    print("="*80)
    
    extractor = LLMExtractor(model_override=model_override)
    print(f"LLM 模型: {extractor.model}")
    if model_override:
        print(f"  (从命令行指定)")
    print()
    
    results = []
    success_count = 0
    
    for i in range(10):
        print(f"[{i+1}/10] 提取中...", end=" ", flush=True)
        start = time.time()
        
        result = extractor.extract_entities_and_relations(text, test_doc)
        elapsed = time.time() - start
        
        if result["success"]:
            data = result["data"]
            num_entities = len(data.get("entities", []))
            num_relations = len(data.get("relations", []))
            success_count += 1
            print(f"✓ 成功 ({elapsed:.1f}s) - 实体:{num_entities}, 关系:{num_relations}")
        else:
            print(f"✗ 失败 ({elapsed:.1f}s) - {result.get('error', 'Unknown')[:50]}")
        
        results.append(result)
        time.sleep(1)  # 避免触发限流
    
    print()
    print("="*80)
    print("统计分析")
    print("="*80)
    
    # 成功率
    success_rate = success_count / 10 * 100
    print(f"\n成功率: {success_count}/10 ({success_rate:.0f}%)")
    
    # 成功的结果详细分析
    if success_count > 0:
        successful_results = [r for r in results if r["success"]]
        
        # 实体数量统计
        entity_counts = [len(r["data"]["entities"]) for r in successful_results]
        relation_counts = [len(r["data"]["relations"]) for r in successful_results]
        
        print(f"\n实体数量范围: {min(entity_counts)}-{max(entity_counts)} (平均: {sum(entity_counts)/len(entity_counts):.1f})")
        print(f"关系数量范围: {min(relation_counts)}-{max(relation_counts)} (平均: {sum(relation_counts)/len(relation_counts):.1f})")
        
        # 实体一致性分析
        print("\n实体一致性分析:")
        all_entity_names = set()
        entity_frequency = {}
        
        for r in successful_results:
            for entity in r["data"]["entities"]:
                name = entity["name"]
                all_entity_names.add(name)
                entity_frequency[name] = entity_frequency.get(name, 0) + 1
        
        # 稳定出现的实体（出现次数 >= 80%）
        stable_threshold = success_count * 0.8
        stable_entities = {k: v for k, v in entity_frequency.items() if v >= stable_threshold}
        
        print(f"  总共发现的不同实体: {len(all_entity_names)}")
        print(f"  稳定实体（≥80%次数出现）: {len(stable_entities)}")
        if stable_entities:
            print(f"  稳定实体列表:")
            for name, count in sorted(stable_entities.items(), key=lambda x: -x[1])[:10]:
                print(f"    - {name}: {count}/{success_count} 次")
        
        # 关系一致性分析
        print("\n关系一致性分析:")
        all_relations = set()
        relation_frequency = {}
        
        for r in successful_results:
            for rel in r["data"]["relations"]:
                # 容错：某些关系可能缺少字段
                head = rel.get('head', 'UNKNOWN')
                tail = rel.get('tail', 'UNKNOWN')
                rel_type = rel.get('type', 'UNKNOWN')
                rel_key = f"{head} -[{rel_type}]-> {tail}"
                all_relations.add(rel_key)
                relation_frequency[rel_key] = relation_frequency.get(rel_key, 0) + 1
        
        stable_relations = {k: v for k, v in relation_frequency.items() if v >= stable_threshold}
        
        print(f"  总共发现的不同关系: {len(all_relations)}")
        print(f"  稳定关系（≥80%次数出现）: {len(stable_relations)}")
        if stable_relations:
            print(f"  稳定关系列表:")
            for rel, count in sorted(stable_relations.items(), key=lambda x: -x[1])[:10]:
                print(f"    - {rel}: {count}/{success_count} 次")
        
        # Token 使用统计
        total_tokens = sum(r.get("tokens", {}).get("total_tokens", 0) for r in successful_results)
        avg_tokens = total_tokens / len(successful_results)
        print(f"\n平均 Token 使用: {avg_tokens:.0f} tokens/次")
        print(f"总 Token 消耗: {total_tokens} tokens")
        print(f"估算成本: ¥{total_tokens * 0.000001:.4f}")
        
        # 保存所有成功结果
        output_dir = base_dir / "hermes-docs-zh-hanlp" / "stability-test"
        output_dir.mkdir(parents=True, exist_ok=True)
        
        for i, r in enumerate(successful_results):
            output_path = output_dir / f"run-{i+1}.json"
            with open(output_path, 'w', encoding='utf-8') as f:
                json.dump(r["data"], f, ensure_ascii=False, indent=2)
        
        print(f"\n所有成功结果已保存到: {output_dir}")
    
    print()
    print("="*80)
    print("测试完成")
    print("="*80)


if __name__ == "__main__":
    main()
