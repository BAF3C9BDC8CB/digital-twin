# dt build --test 设计

> 状态：实施中 | 日期：2026-07-22

## 一、目标

`dt build --test` 采样少量真实数据，以 `test-` 前缀隔离构建，自动验证管线正确性后清理。

## 二、采样策略

| 类别 | 采样数 | 规则 |
|------|--------|------|
| 代码项目 | 1 | 文件最少的项目，限 20 个文件 |
| Nacos 配置 | 3 | 不同 group，各取 1 条 |
| K8s Pod | 2 | 不同 namespace，各取 1 条 |
| Jenkins Job | 1 | 第一条 |
| Knowledge | 1 | 最新一条 |
| Decision | 1 | 最新一条 |

## 三、test- 命名规则

`test-` + 原节点标签。例：`:Class` → `:test-Class`，Qdrant `{proj}_semantic` → `test-{proj}_semantic`。

## 四、执行流程

```
1. 发现+采样 → 2. 清理旧test数据 → 3. 构建test数据 → 4. 验证 → 5. 清理(unless --keep)
```

## 五、验证项

- test-Class/Method 节点数 > 0
- test-NacosConfig/test-Pod/test-JenkinsJob 节点存在
- 向量搜索返回结果

## 六、命令

- `dt build --test` 完整流程
- `dt build --test --keep` 保留数据
- `dt clean --test` 清理

## 七、代码结构

`src/application/pipeline/test/{mod,runner,sampler,builder,validator,report,cleanup}.rs`
