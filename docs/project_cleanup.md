# 项目 build 清单

> 2026-07-07 | Neo4j 32 项目 | Qdrant 43 集合 | 已匹配 24

---

## 需 build 的项目

### Neo4j 有项目无向量（8）

```bash
dt build --path /data/aflmProjects/shopyijianbao_shop --name shopyijianbao
dt build --path /data/aflmProjects/yijianbao/yingchao_web --name yingchao_web
dt build --path /data/aflmProjects/charts-prod --name charts-prod
dt build --path /data/myProject/digital-twin --name digital-twin
dt build --path /data/aflmProjects/aflm/admin/uvp-admin-center --name uvp-admin-center
dt build --path /data/aflmProjects/yijianbao --name yijianbao
dt build --path /data/aflmProjects/yiyuantong --name yiyuantong
```

### Qdrant 孤集合 — 需补建 Neo4j 节点后 build（19）

```bash
dt build --path /data/aflmProjects/others/pay --name pay
dt build --path /data/aflmProjects/others/uvp-cache-center --name uvp-cache-center
dt build --path /data/aflmProjects/unimportant/uvp-charge-center --name uvp-charge-center
dt build --path /data/aflmProjects/others/uvp-config-center --name uvp-config-center
dt build --path /data/aflmProjects/unimportant/uvp-data-center --name uvp-data-center
dt build --path /data/aflmProjects/unimportant/uvp-log-center --name uvp-log-center
dt build --path /data/aflmProjects/unimportant/uvp-log-server --name uvp-log-server
dt build --path /data/aflmProjects/unimportant/uvp-saas-warehouse --name uvp-saas-warehouse
dt build --path /data/aflmProjects/others/uvp-search-center --name uvp-search-center
dt build --path /data/aflmProjects/others/uvp-settlement-center --name uvp-settlement-center
dt build --path /data/aflmProjects/others/uvp-sms-center --name uvp-sms-center
dt build --path /data/aflmProjects/others/uvp-statistics-center --name uvp-statistics-center
dt build --path /data/aflmProjects/unimportant/yimeng-website --name yimeng-website
dt build --path /data/aflmProjects/warehouse/yyc-caigou --name yyc-caigou
dt build --path /data/aflmProjects/warehouse/yyc-yaochang-gongsi --name yyc-yaochang-gongsi
```

> `uvp-inner-intergration` 的 Qdrant 集合已存在，Neo4j 节点之前被误删，需一并进行 build：
```bash
dt build --path /data/aflmProjects/uvp-inner-intergration --name uvp-inner-intergration
```

---

共 **24** 条 build 命令。`dt build` 会自动检测增量，已有向量的项目会跳过。
