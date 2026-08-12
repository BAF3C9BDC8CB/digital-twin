# Nacos 配置搜索 UX — 用户目标 vs 现状缺口（2026-08-07 实测）

背景：用户评审 `docs/plans/nacos-llm-first-implementation-plan.md` 时澄清真实需求 —— 不是那份方案的 LLM-first 语义分块契约，而是：
**Nacos 配置与普通项目/目录构建走同一套处理方式（统一 VirtualFile pipeline），搜索结果渲染与普通结果一致但呈配置形态。**

## 用户给出的目标样式

```
[配置/ConfigKey] spring.datasource.url
  分析: 配置项用途的规则摘要，或暂显示"暂无摘要"
  来源: dt://nacos/test/{namespace}/{group}/{dataId}#spring.datasource
  正文:
      spring:
        cloud:
          nacos:
            discovery:
              server-addr: nacos-headless.nacos-test.svc.cluster.local:8848     ## nacos-headless...
              namespace: af6d04ec-7142-47af-89bf-0e6009f40bc1  #命名空间 代指某个环境
```

要点：正文与原文档保持一致（某一段），缩进/注释逐字符保留。

## 现状验证（2026-08-07 实测，release 二进制）

`dt search "spring.cloud.nacos.discovery server-addr" --world config --limit 3` 实际输出：

```
[0.0320] [ConfigChunk] [common.yaml:spring.cloud] (2 keys)
  正文:
    spring:
      cloud:
        nacos:
          discovery:
            server-addr: nacos-headless.nacos-test.svc.cluster.local:8848     ## nacos-headless...
            namespace: af6d04ec-7142-47af-89bf-0e6009f40bc1  #命名空间 代指某个环境
  分析: 配置 Spring 应用的运行参数和组件行为。
  来源: dt://nacos/test/test/DEFAULT_GROUP/common.yaml#section=spring.cloud
```

Qdrant `config_chunks` payload 实测（1607 点）：字段为 `config_type, data_id, environment, group, key_count, namespace, section_name, source_type, text`；`text` 保留原始缩进与 `#`/`##` 注释（repr 验证）；`environment` 普遍为空字符串；**无 `resource_type` 字段**。mysql 相关 127 点中含 10 条 pagehelper/helper-dialect 弱匹配（方案要解决的痛点真实存在）。

## 缺口对照表

| 项 | 现状 | 目标 | 缺口位置 |
|---|---|---|---|
| 标签 | `[ConfigChunk]`（裸 entity_type） | `[配置/ConfigKey]` | config 路径未填 `file_type_label`（postprocess_hits 对 dt://nacos source_ref 未推导 file_type）；entity_type 粒度是 Chunk 不是 Key |
| 标题 | `[data_id:section] (N keys)` | 具体配置项 key（如 `spring.datasource.url`） | `search_config.rs` 构造 title 处（vector 路径 ~L237，Cypher 回退 ~L293） |
| 分析 | `config_purpose_summary()`（search_config.rs:123）**硬编码关键字规则**，只识别 nacos/datasource/redis/kafka/log/spring 6 类，其余兜底"配置应用相关组件的运行参数" | 逐配置项用途摘要，或"暂无摘要" | 换 LLM 摘要需走 `llm_analysis` 字段（SearchHit 已有该字段，config 路径目前只填规则文案） |
| 正文 | 整段 section 原文返回，缩进/注释保留 | 同 | ✅ 已满足 |
| 来源 | `dt://nacos/{env}/{ns}/{group}/{data_id}#section={section}`（env 空时硬编码回退 "test"，search_config.rs:240） | `dt://nacos/{env}/{namespace}/{group}/{dataId}#{section}` | 基本一致；注意 env 空值回退逻辑 |

## 结论

真实改动面很小，全部在 `src/application/context/search_config.rs` + `search_render.rs` 渲染分支：
1. title 改为列出具体 keys（或首 key 代表）
2. `file_type_label` 填 "配置"
3. `config_purpose_summary` 替换/增强（可选接 LLM，或先扩充规则集）
无需新建 LLM-first 分块管线；与 Phase 0 统一架构（VirtualFile）方向正交兼容。
