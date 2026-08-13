# 搜索关键词通道修复(2026-08-13):中文切词 + 配额分配 + 跨项目过滤

## 背景与根因
- `keywords_of`(retrieve.rs)/`extract_keywords`(search_mcp.rs)对中文不切分:
  `is_alphanumeric() || !is_ascii()` 把所有中文连全角标点累积成一个"词" → kw=整句 → CONTAINS 0 行。
- 实测:知识/代码/文档三世界关键词通道全部归零,混合检索退化为纯向量+图扩展。
- 修复后按"搜索结果更准确"目标,共 6 项改动(commit cb3828c)。

## 改动清单
1. **keywords_of 重写**(单一事实源,search_mcp 委托引用):
   - 三态扫描:ASCII 段(小写、≥2字,权重5)/ CJK 段(虚词表最长优先切分)/ 分隔符(含全角标点,flush 两段)
   - CJK 子段:2-4字整段(w4)+前缀bigram(w3)+前缀tri/4-gram(w2)+内部bigram(w1)
   - 去重保最高权重 → weight desc → pos asc → len desc → truncate(max)
   - ⚠️ 切片必须按 char 边界(Vec<char> 索引),禁止按字节切 UTF-8
2. **keyword_recall 加 project 过滤**:WHERE `AND e.project = '{p}'`(Some 时拼接,None 不加)——Entity 有 project 属性,无过滤会跨项目污染(切词变短后"注册"命中所有项目同名实体)
3. **补 elementId(e) AS seed_element_id**:代码在读但 RETURN 从未返回 → 非 Entity type 的 kw 种子无法图扩展(静默丢失)
4. **WHERE 扩展 keywords List**:`OR any(k IN coalesce(e.keywords, []) WHERE ...)`——必须用 any(),Memgraph toString(List)=null 会废掉整条 WHERE
5. **命中质量分级**:match_kind exact=0.95/prefix=0.90/substr=0.80;ORDER BY 精确>前缀>子串;LIMIT 50;强制保留仅限 semantic≥0.90(exact/prefix),substr 正常参与 rerank 竞争
6. **每 kw 配额分配**:`per_kw_cap = limit/kws.len()`,全部 kw 都发查询(修复前高权重 kw 先到先得独占 limit,如"git 注册逻辑"只出 git 不出注册——实测 Q1 的 git 独占把"注册"挤出 kw top3)
7. **search_render:world=all 跳过低分提示**:RRF 排名分=1/(60+rank) top1 恒≈0.0164 < 0.5 阈值,阈值仅对单世界语义分有效

## 虚词表设计要点(踩坑)
- 高置信度意图词入表:为什么/怎么/如何/是否/可以/需要/应该/进行/无法/不能/查看/对比/组建/分析 等
- ⚠️ 内容语义强的词**不可**入表:配置/测试/设置/查询/发布/部署/调用/实现 等——"配置中心""测试环境"会被误拆,损失精确匹配(实测判断后回退)
- 否定词(无法/不能)入表后内容词(注册)才以整段高权重独立:"无法注册"→切"无法"+"注册"
- 单字虚词含 '不'(修饰词);代码域内容词(注册/缓存/支付)绝不入表

## 端到端实测对比(改进前→后)
| 查询 | 改进前 | 改进后 |
|------|--------|--------|
| redis缓存怎么用(knowledge) | kw rows=0 | Redis缓存 0.9174 / Redis 0.915,kw redis→34、缓存→50 |
| 无法注册...(knowledge) | kw rows=0,top 注册页面 0.4294 | kw git→50、注册→50,注册表/注册页面进 top |
| 创建订单(code) | createOrder 0.768(基线正常) | 保持 0.768,无回归 |
| 创建订单(world=all) | 低分提示误报(0.0164<0.5) | 提示消除(RRF 路径跳过) |

## 验证方法
- cargo test:673 passed / 0 failed(含 21 个新单测:切词矩阵/配额分配/project 过滤/match_kind 分级/渲染告警)
- 端到端:固定查询集 before/after 对比,日志 grep `keyword_recall: kw=... rows=...` 验证通道恢复
- 关键日志:搜索完成信息含 total/per_world,degraded=[] 表示 rerank 可用

## 遗留/后续
- rerank 权重 0.6 主导融合,查询含多个概念时(git+注册)rerank 模型判断决定排序——如需强化核心意图词,可调融合权重或查询意图提取(更大手术,未做)
- 中文切词仍是启发式(无词典):专有名词边界靠 n-gram 兜底;未来联网后可升级 jieba-rs
