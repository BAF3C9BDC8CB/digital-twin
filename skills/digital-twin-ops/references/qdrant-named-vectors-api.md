# Qdrant named vectors（双向量）搜索/展示层 API 备忘

digital-twin-v2 双向量架构（用户拍板）：base 向量只做**召回**，llm 向量做 **rerank**。
全部签名在 `~/.cargo/registry/src/index.crates.io-*/qdrant-client-1.18.0/src/` 源码核实（2026-08-11）。

## 架构与 payload 契约
- base 向量 = embed(signature+comment)，确定性（不依赖 LLM），必填。
- llm 向量 = embed(llm_analysis 文本)，仅 llm_status=success 后写入（Phase 2），可选；失败写 llm_status=failed + llm_retries++，补偿自愈 backfill。
- 常量 `src/shared/collections.rs`：`VECTOR_NAME_BASE="base"`、`VECTOR_NAME_LLM="llm"`、集合 `code_methods`。
- payload：name/signature/class_name/file_path/package_or_module/language/project/start_line/end_line/params/return_type/calls/comment/entity_id + llm_status + llm_analysis + llm_retries。
- 展示层（render_hit Method 分支）：llm_analysis 非空 → "分析: 用途：…/逻辑：…"；空/缺失（llm_status=failed 或缺失）→ **"分析: 暂无 LLM 分析"**，不再回退到 snippet（snippet 是 "file:Ls-e" 位置串）；"位置: file:Ls-e [signature]" 单独一行保留。hit_from_payload 把 llm_analysis 空串归一化为 None（与 config 世界对齐）。

## 关键 API（qdrant-client 1.18.0）
- **named vector 召回**：`SearchPointsBuilder::new(collection, vector, limit).with_payload(true).vector_name("base")`（builder_ext.rs L65 的 new + search_points_builder.rs L89 vector_name）。
- **搜索响应不含向量**：repo.rs `scored_points_to_json` 只出 `{id, score, payload}` → llm 向量必须按点 id 另拉。
- **按 id 拉向量**：`GetPointsBuilder::new(collection, ids: Vec<PointId>).with_payload(false).with_vectors(VectorsSelector{names: vec!["llm".into()]})`（builder_ext.rs L128；client 方法 `qdrant.get_points`）。PointId 用 `PointId{point_id_options: Some(point_id::PointIdOptions::Num(n))}`。
- **响应解析链**：`RetrievedPoint.vectors → VectorsOutput.vectors_options → vectors_output::VectorsOptions::{Vector(VectorOutput) | Vectors(NamedVectorsOutput)}`；`NamedVectorsOutput.vectors: HashMap<String, VectorOutput>`；取数据用 `vector_output::Vector::Dense(DenseVector.data)`（新字段），**`VectorOutput.data` 已 deprecated 勿用**（无 deny(warnings)，但保持干净）。
- **with_vectors 选择器**：`Into<with_vectors_selector::SelectorOptions>` 只支持 `bool`（全量/无）与 `VectorsSelector{names}`（白名单）——传名字列表要构造 VectorsSelector。

## 陷阱
- **repo.rs 的 search/search_with_filter 是 doc/knowledge/config 世界共用**（那些是单向量集合）——不能直接给它们加 `.vector_name("base")`。正确做法：trait（domain/traits.rs）新增 `search_by_vector_name(collection, vector, limit, vector_name, filter: Option<Value>)`，默认实现回退到 search/search_with_filter（现有 StubVector/NoopVectorRepo 测试零改动），QdrantRepo 覆写时才加 vector_name；rerank 拉向量同理新增 `get_vectors_by_ids(collection, ids: Vec<u64>, vector_name)` 默认返回空。trait 加带默认实现的方法不破坏任何既有 impl。
- **rerank 融合**：`final = 0.5*base_score + 0.5*cosine(query_vec, llm_vec)`；无 llm 向量 → final=base_score。只对 top-N=50 批量拉向量；精确通道命中（固定 0.95 置顶）不参与 rerank；rerank 在 search_code 内、跨世界 RRF（fusion.rs 按 world:id 去重）之前做，不破坏融合。
- **id 类型坑**：搜索响应 hit 的 id 是数值点 id（字符串化 u64），关键词兜底命中的 id 是 entity_id 字符串——按 id 拉向量前先 `parse::<u64>()` 过滤，兜底命中天然跳过。

## 工具链小坑（实测）
- read_file 对含 CJK 的 UTF-8 文件可能误报 "Binary file"（`file` 命令确认是纯文本）：用 python 带行号 dump 到 /tmp 再 read_file。
- bash 的 `$'\x00'` 会展开成空串（bash 变量存不了 NUL）——`grep -c $'\x00'` 实际数所有行，不能检测 NUL；用 `python3 -c "open(f,'rb').read().count(b'\x00')"`。
- terminal 对 `sed -n "$(grep ... | cut ...)"` 命令替换复合命令触发 blocklist——改用 `grep -n -A N file` 上下文输出。
