# Builder/构造器噪声优化（2026-08-12 导出分析第三轮发现，未实施）

## 问题
Lombok `@Builder` 注解生成样板 `builder()` 方法，Phase 2 LLM 对它们生成无意义分析
（"用途：暂无"类低质文本），污染语义检索——搜业务语义时这些低质量描述向量参与打分，
挤占高质量方法/类结果。此前"优化方案综合设计员"任务书第 5 项（数据层）已点名此问题，
但一直未落地。

## 量化方法（可复现）
```python
from qdrant_client import QdrantClient
from qdrant_client.http import models as qm
qc = QdrantClient(url='http://127.0.0.1:6333')
filt = qm.Filter(must=[qm.FieldCondition(key='project', match=qm.MatchValue(value='im-center'))])
names = {'builder': 0, 'constructor': 0, 'other': 0}
offset = None
while True:
    pts, offset = qc.scroll('code_methods', limit=1000, offset=offset,
                            with_payload=['name','llm_status','signature','class_name'],
                            with_vectors=False, scroll_filter=filt)
    for p in pts:
        n = p.payload.get('name',''); sig = str(p.payload.get('signature',''))
        cls = p.payload.get('class_name','')
        if n == 'builder': names['builder'] += 1
        elif sig.strip().startswith(cls) and '(' in sig: names['constructor'] += 1
        else: names['other'] += 1
    if not offset: break
total = sum(names.values())
print(f'{total=} builder={names["builder"]} ({names["builder"]*100/total:.1f}%)')
```
⚠️ 全库 scroll 会因集合大（31085 点）被 5000 limit 截断——**必须带 scroll_filter 按 project 过滤**，
否则统计漏掉深处的项目方法。

## im-center 实测（2026-08-12）
- 2287 方法中 **93 个 builder（4.1%）**，构造器 1 个
- llm_status=success 100%（LLM 全给这些样板生成了无意义分析）

## 优化方案
### P0：Phase 2 跳过 builder LLM 调用（src/application/build/pipeline.rs）
检测 `name == "builder"`（且 class_name 非空）→ 跳过 LLM，写 `llm_status = 'skipped_builder'`。
- 省 token：93 方法 × ~200 token ≈ 18.6K token/构建
- 状态位语义：skipped ≠ failed（failed 会进重试循环浪费 LLM）

### P1：检索侧降权（src/application/context/search_mcp.rs）
search_code 对 `name == "builder"` 或 `llm_status == "skipped_builder"` 的命中降权
（score × 0.5）或默认不参与向量检索（仅精确检索可达）。

### P2：构造器同样处理（数量少，优先级低）

## 验证
- 构建后统计 `llm_status = 'skipped_builder'` 数量 = 预期 builder 数
- 检索结果中 builder 方法占比下降

## 报告
/data/myProject/digital-twin-v2/reports/2026-08-12-export-analysis-round3-builder-noise.md
