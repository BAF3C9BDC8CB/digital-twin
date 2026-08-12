# code_classes 类实体检索：功能回归配方（2026-08-12 实测，P1-4 修复后更新）

## 背景
Phase 2.6 后 Class 实体向量化进 Qdrant `code_classes` 集合（im-center 项目 357 点），
`dt search --world code` 可检索到类（entity_type=Class）。本文件是类实体检索的功能验证配方。

## 功能回归配方（5 查询 + 点数核对）
工作目录须为项目根 `/data/myProject/digital-twin-v2`。全部 5 项 + Qdrant 点数应在 1 分钟内跑完：

```bash
# 1) 语义中文查询应命中 GroupController [Class]（≤8 内即可）
dt search "群组控制器 群组消息" --world code --project im-center --limit 8 --json
# 2) 应首条命中 SourceHolder [Class]
dt search "线程本地字符串管理" --world code --project im-center --limit 5 --json
# 3) 类实体应参与（实测 5/5 为 Class，MessageRecordMongoService rank 3）
dt search "数据库操作 消息记录" --world code --project im-center --limit 5 --json
# 4) 不带 project 全库搜索，不 panic 不 WARN
dt search "im-center" --world code --limit 5
# 5) 精确类名应首条置顶 [Class] 0.95（P1-4 修复后）
dt search "GroupController" --world code --project im-center --limit 5

# 点数核对（应 357）
python3 -c "from qdrant_client import QdrantClient; qc=QdrantClient(url='http://127.0.0.1:6333'); print(qc.get_collection('code_classes').points_count)"
```

## 实测结果（2026-08-12，P1-4 修复后复测）
①GroupController [Class] rank 7/8（前 3 是 GroupController.java 的方法点，属排名偏差非 bug）；
②SourceHolder rank 1 达标；③5/5 Class 达标；④正常；⑤**GroupController [Class] rank1 score=0.950 置顶 ✅（P1-4 修复前为缺口）**；
点数 357 达标。

## 历史缺口（P1-4 已修复，2026-08-12）
- **此前缺口**：精确类名检索（测试5）未命中类实体——1A 精确通道跳过 code_classes
  （单向量集合无 named vectors），类名精确匹配完全依赖向量相似度 ≥ min_score。
- **修复**：1A 通道 is_classes 分支改用单向量版 `search_with_filter`（search_mcp.rs），
  类名精确命中给 0.95 置顶。关键词兜底也循环两个集合（code_methods + code_classes）。
- **验证**：`dt search "GroupController" --world code --project im-center` → rank1 0.950。

## 相关坑
- Qdrant 单向量集合不能用 named 查询/写入（"Not existing vector name"）；
  类集合用 `search_with_filter`（单向量版），方法集合用 `search_named_with_filter`。
- 类点 payload：entity_type=Class + llm_analysis(描述) + file_path + project + name。
