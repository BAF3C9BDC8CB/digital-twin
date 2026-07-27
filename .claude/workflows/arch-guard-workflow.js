export const meta = {
  name: 'arch-guard-workflow',
  description: '独立架构检查：验证 DDD 层边界合规性，不修改任何代码',
  phases: [
    { title: 'Architecture Check', detail: '扫描代码库，检查层边界违规' },
  ],
};

// ──────────────────────────────────────────────
// Schema
// ──────────────────────────────────────────────
const ARCH_REPORT_SCHEMA = {
  type: 'object',
  properties: {
    total_files_scanned: { type: 'number' },
    violations: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string', enum: ['error', 'warning'] },
          file: { type: 'string' },
          line: { type: 'number' },
          message: { type: 'string' },
        },
        required: ['severity', 'file', 'message'],
      },
    },
    summary: { type: 'string' },
  },
  required: ['total_files_scanned', 'violations', 'summary'],
};

// ──────────────────────────────────────────────
// Phase 1: Architecture Check
// ──────────────────────────────────────────────
phase('Architecture Check');
log('🔍 扫描代码库，检查 DDD 层边界违规...');

const archReport = await agent(
  `你是架构守卫。请对当前代码库进行完整的层边界检查。

## 层边界规则

| 文件在... | 允许引用 | 禁止引用 |
|-----------|---------|---------|
| src/domain/ | crate::domain::* | infrastructure/, application/, interfaces/ |
| src/infrastructure/ | domain/, shared/ | application/, interfaces/ |
| src/application/ | domain/, infrastructure/, shared/ | interfaces/ |
| src/interfaces/ | 所有层 | 无 |
| src/shared/ | domain/ | infrastructure/, application/, interfaces/ |

## 检查方法

1. 扫描 src/ 下所有 .rs 文件
2. 提取每个文件的 use crate::* 导入
3. 根据文件所在层判断是否违规
4. 特别关注：domain/ 引用 infrastructure/ 是最严重的违规

## 输出要求

- 列出所有违规（带文件路径和行号）
- 如果没有违规，标记 summary 为 "✅ 所有层边界合规"
- 如果有违规，标记 summary 为违规摘要

注意事项：
- src/main.rs 是 composition root，可以引用所有层（不做检查）
- src/lib.rs 是 crate root，可以引用所有层（不做检查）
- 检查的重点是 src/domain/, src/infrastructure/, src/application/, src/shared/ 四个核心层`,
  { label: 'arch-check', phase: 'Architecture Check', schema: ARCH_REPORT_SCHEMA }
);

// ──────────────────────────────────────────────
// 输出报告
// ──────────────────────────────────────────────
log(`📊 扫描了 ${archReport.total_files_scanned} 个文件`);

if (archReport.violations.length === 0) {
  log('✅ 层边界合规性检查通过！');
} else {
  log(`⚠️ 发现 ${archReport.violations.length} 个违规:`);
  archReport.violations.forEach(v => {
    const icon = v.severity === 'error' ? '❌' : '⚠️';
    log(`  ${icon} [${v.severity}] ${v.file}:${v.line || '?'} — ${v.message}`);
  });
}

log('');
log(archReport.summary);

return {
  total_files_scanned: archReport.total_files_scanned,
  violations_count: archReport.violations.length,
  violations: archReport.violations,
  summary: archReport.summary,
};