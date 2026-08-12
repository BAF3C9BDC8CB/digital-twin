# SiliconFlow 模型广场选型 / 价格采集(2026-08-08 实测)

用户常需要在硅基流动模型广场(siliconflow.cn/models)筛选"性价比高"的对话模型。
本文档记录无登录抓取价格/上下文/尺寸的方法 + 2026-08-08 全量价格快照 + 选型结论。

## 访问与登录边界(重要)

- **列表页 `https://siliconflow.cn/models` 公开可访问,无需登录**(模型中心 | 硅基流动)。
- **详情页需登录**:点击任意模型卡片 → 跳转 `account.siliconflow.cn/zh/login?redirect=...`。
  所以 benchmark/评测分数拿不到,只能拿卡片上的价格/上下文长度/尺寸。
- `https://siliconflow.cn/models/<vendor>/<name>` 直链会 302 回首页,别用直链。
- `cloud.siliconflow.cn/models` 也强制登录(统一登录页),模型广场用 www 域即可。

## 抓取方法(Next.js SSR 页面)

页面是 Next.js(数据在 `window.__next_f` 里,解析麻烦),直接翻页 + DOM 提取最稳:

1. 列表默认 5 页 × 每页 20 卡,底部是 ant-design 分页(`li.ant-pagination-item`)。
2. **分页坑**:`browser_click` 点页码 ref 经常不生效(点击后 active 仍是 1);改用 JS 直接触发:
   ```js
   document.querySelectorAll('li[class*="ant-pagination-item"]')[pageIdx].click();
   // 然后 await sleep(~1.5s) 再提取,否则 grid 未更新
   ```
   切换成功标志:`li[class*="ant-pagination-item-active"]` 的 textContent = 目标页码。
3. 卡片容器:`div.grid.grid-cols-4`(只有一个 children>=10 的 grid),直接子元素即卡片。
   第 5 页只有 7 张卡,`filter(g=>g.children.length>=10)` 会 miss → 用 grid-cols-4 class 选择。
4. 卡片提取(对 card.textContent 压缩空白后正则):
   ```js
   const nameM = t.match(/([A-Za-z0-9_-]+(?:\/[A-Za-z0-9_.-]+)+)/);   // vendor/model
   const priceIn = (t.match(/输入:\s*([^输]+?)\s*输出:/) || [,'',''])[1];
   const priceOut = (t.match(/输出:\s*([^上]+?)\s*(?:上下文|$)/) || [,'',''])[1];
   const ctx = (t.match(/上下文长度:\s*([^\s]+)/) || [,'',''])[1];
   const size = (t.match(/尺寸：([^\s]+)/) || [,'',''])[1];
   ```
   注意卡片描述正文也含"尺寸:"等词,名称正则取 vendor/model 格式最可靠;描述里可能重复出现多个型号名,取第一个匹配。

## 2026-08-08 价格快照(对话类,输入/输出 ￥/M tokens)

**基准:Qwen/Qwen3-14B = ￥0.5 / ￥2,128K 上下文,稠密 14B。**

⚠️ **Qwen3.5 系列没有 14B 型号**(只有 4B/9B/27B/35B-A3B/122B-A10B/397B-A17B)。
用户说"qwen3.5 14B"时,实际指 Qwen3-14B(Qwen3 系,2025-04 发布)。

高性价比候选(相对 Qwen3-14B,按价格带):

| 模型 | 输入/输出 | 上下文 | 参数 | 点评 |
|---|---|---|---|---|
| deepseek-ai/DeepSeek-V4-Flash | 1 / 2 | 1024K | 284B MoE(13B act) | 输出同价,性能碾压,首选 |
| stepfun-ai/Step-3.5-Flash | 0.7 / 2.1 | 256K | 196B MoE | 几乎同价,阶跃最新代 |
| Qwen/Qwen3.5-397B-A17B | 2 / 1.2 | 256K | 397B MoE(17B act) | 输出比 14B 便宜,长输出场景划算 |
| deepseek-ai/DeepSeek-V3.2 | 2 / 3 | 160K | 671B | 旗舰级推理(IMO 金牌),质量优先 |
| nex-agi/Nex-N2-Pro | 1.75 / 7 | 256K | 397B | SWE-Pro 开源 SOTA,编码/Agent |
| inclusionAI/Ling-mini-2.0 | 0.5 / 2 | 128K | 16B | 与 14B 完全同价,直接平替 |
| Qwen/Qwen3-30B-A3B-Instruct-2507 | 0.7 / 2.8 | 256K | 30B MoE(3B act) | 微涨,256K 上下文 |
| Qwen/Qwen3-Coder-30B-A3B-Instruct | 0.7 / 2.8 | 256K | 30B MoE | 代码专用同价位 |
| Qwen/Qwen3-32B | 1 / 4 | 128K | 32B 稠密 | 2 倍价,明显更强 |
| Qwen/Qwen3.6-35B-A3B | 1.6 / 12.8 | 256K | 35B MoE(3B act) | 响应快但输出贵 |
| MiniMaxAI/MiniMax-M2.5 | 2.1 / 8.4 | 192K | 229B | SWE-Bench 80.2%,输出 4x 贵 |
| 免费档 Qwen3-8B / Qwen3.5-4B / GLM-Z1-9B / GLM-4-9B / Qwen2.5-7B | 0 / 0 | — | — | 简单任务白嫖 |

避坑:Qwen3.5-27B(输出 ￥14.4)、Qwen3.5-9B(推理模型,content 恒空 — 见主 SKILL.md SiliconFlow 陷阱)、
Qwen3.5-122B-A10B(输出 ￥16)。生图/嵌入/重排序/视频型号列表页也有,选型时按卡片类型标签过滤。

## 选型方法论

- 无登录时性能只能靠 参数规模 + 代际 + 上下文 推断(卡片上有尺寸字段);要 benchmark 需登录详情页。
- 性价比 ≈ 能力档位 / (输入价+输出价);用户场景不同结论不同:
  输出密集型 → 397B-A17B(输出 1.2);快响应高并发 → V4-Flash / 30B-A3B;
  同价平替 → Ling-mini-2.0 / Step-3.5-Flash;质量优先 → DeepSeek-V3.2。
- 价格快照会过期,先按上文方法现抓再下结论,别直接引用本文档价格当最新值。
