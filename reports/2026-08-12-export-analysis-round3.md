# 导出分析 — 剩余优化点（2026-08-12 第三轮）

**分析对象**：用户重新导出的 default profile（/tmp/default-export，40MB，81 会话 + skills + SOUL + memories + config）
**结论**：导出与本机完全同步（SOUL.md/MEMORY.md diff 为空；skills 已覆盖场景 D/Phase 2.6/read_query 别名坑/精确检索）
**剩余优化点**：4 个，均为"知识图谱能力未完全发挥"的真实差距

---

## GAP-A：Class 无向量 → code 世界检索不到类（最有价值）

**实测证据**：
- `dt search "SourceHolder" --world code --project im-center` → 只命中同名方法（getAddSource/setAddSource），**SourceHolder 类本身搜不到**
- 类已有 description（Phase 2.6 生成）但**没进 Qdrant**——code 世界向量只索引方法

**影响**：agent 用中文查类（如"线程本地字符串管理"）或精确类名，dt_search_kg(world=code) 无法召回类实体——类描述白生成，检索价值未兑现。

**方案**：Phase 2.6 成功后，把类也 upsert 进 Qdrant（新建 `code_classes` 集合或复用 code_methods，payload: name/class_id/file_path/project/description，向量=description 文本）。成本 M。

## GAP-B：LIMIT 300 每轮上限 → 大项目需多轮

**实测**：357 类分两轮（300+57）；更大项目（数千类）要跑 N 轮，每轮都要重新扫描 + 过滤已 success 的。

**方案**：改为循环处理（每轮 300，直到 jobs 空）或按缺口比例动态分批。成本 S。

## GAP-C：LLM provider 稳定性拖慢构建

**实测**：今天多次 APIConnectionError / 5xx / 空响应（方案设计员失败、3 个会话 max_retries_exhausted、构建重试风暴）。重试机制兜住了，但 357 类描述花了 ~15 分钟。

**方案**：考虑 provider 降级链（opencode-go → 备用）或把类描述补偿挪到构建后异步任务（用户不感知）。成本 M，外部依赖。

## GAP-D：子代理 KG 使用未验证

**现状**：AGENTS.md 已加委派准则（任务书带 world=code+project），但无机制验证子代理是否遵循。

**方案**：下一次团队任务时检查子代理日志的 dt_search_kg 调用参数（参照 check-dt-usage.sh 思路）。成本 S（验证性）。

---

## 建议优先级

| 优先级 | 项 | 理由 |
|--------|-----|------|
| P0 | GAP-A Class 向量化 | 类描述已生成但检索不到=最大浪费 |
| P1 | GAP-B LIMIT 循环 | 大项目多轮体验差 |
| P1 | GAP-D 子代理验证 | 下一团队任务顺带验证 |
| P2 | GAP-C provider 稳定性 | 外部依赖，重试已兜底 |

## 验证方式（GAP-A 实施后）
1. `dt search "SourceHolder" --world code --project im-center` → 应命中 Class 实体（score >0.7）
2. `dt search "线程本地字符串管理" --world code --project im-center` → 应命中 SourceHolder 类
3. dt health 索引对账一致（新集合纳入对账或明确排除）
