# dt-sense 插件实施记录（2026-08-11 已落地）

## 现状（已实施完成）

- **源码位置**: `/data/myProject/digital-twin-v2/plugins/dt-sense/`（与 mcp/ skill/ 同模式，项目内管理）
- **软链接**: `~/.hermes/plugins/dt-sense` → 项目 `plugins/dt-sense/`
- **config.yaml**: `plugins.enabled: [dt-sense]` + `hooks_auto_accept: true`
- **SOUL.md**: 已追加「知识图谱(KG)感知准则」段（三层漏斗 + 搜索时机决策清单 + 降级禁止）
- **验证**: `hermes plugins list` 显示 dt-sense enabled；端到端模拟 Hermes 加载器（hermes_plugins.dt_sense）→ register → pre_llm_call 注入 [DT-SENSE] 简报 ✓
- **gateway**: 已重启，feishu 连接正常

## 实现要点（踩坑记录）

1. **DT_BIN 默认路径**: `/home/luis/.local/bin/dt`（不是 /usr/local/bin！dt 软链在此）
2. **项目匹配用词边界正则** `(?<![A-Za-z0-9_-])name(?![A-Za-z0-9_-])`：
   - 防 `svc` 误中 `svc-order`（svc 是注册项目名）
   - 防 `update` 里的 `dt` 误匹配别名
   - 最长匹配优先（嵌套项目 warehouse-api > warehouse）
3. **别名表**: user-center→uvp-user-center、warehouse→warehouse-center、dt→digital-twin-v2 等
4. **注册表**: `~/.config/digital-twin/config.yaml` 的 projects.base+items（65 个项目），PyYAML 解析
5. **每会话一次**: is_first_turn + session_id 内存缓存（_seen_sessions），非首轮 return None
6. **fail-open**: 任何异常 return None，绝不 crash agent；dt sense 超时 8s
7. **渲染模板**: [DT-SENSE] 首行 + path/stats/brief/注册项目数 + 固定"搜索触发/禁止"两行，实测 376-474 chars ≤ 1.5KB
8. **hermes config set plugins.enabled** 会把数组存成字符串 `'["dt-sense"]'`——需手动改回 YAML list 格式

## 关键机制

- pre_llm_call hook 的 kwargs 含 `user_message / is_first_turn / session_id`（turn_context.py:1058）
- 注入进 user message（非 system prompt），保 provider prompt cache
- shell hook 的 extra 也带 user_message（shell_hooks.py:541）
- Hermes 插件加载器用 `spec_from_file_location(module_name, init_file)` 按路径加载——软链接目录没问题

## 回退方法

1. `rm /home/luis/.hermes/plugins/dt-sense`（软链）
2. config.yaml 删 `plugins.enabled` 里的 dt-sense + hooks_auto_accept
3. SOUL.md 删 KG 感知准则段
4. 重启 gateway（CLI 新会话即时生效）

## 方案文档

- 总纲: `docs/superpowers/specs/2026-08-11-hermes-kg-integration-plan.md`
- 搜索时机规则: `docs/superpowers/specs/2026-08-11-kg-search-decision-rules.md`
- 成本收益: `docs/superpowers/specs/2026-08-11-kg-search-cost-benefit-model.md`
- 注入模板/SOUL 文本: `docs/superpowers/specs/2026-08-11-pre-llm-call-injection-final.md`
- 查询策略: `docs/superpowers/specs/2026-08-11-kg-auto-query-strategy.md`
