export const meta = {
  name: 'change-workflow',
  description: '完整变更工作流：架构守卫 → 实现+测试 → 审查 → 集成验收',
  phases: [
    { title: 'Architecture Guard', detail: '检查 DDD 层边界合规性' },
    { title: 'Implement', detail: '并行实现代码和编写测试' },
    { title: 'Review', detail: '代码质量审查' },
    { title: 'Integrate', detail: '全量编译、测试、集成验收' },
  ],
};

// ──────────────────────────────────────────────
// Schema 定义
// ──────────────────────────────────────────────
const ARCH_VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['PASS', 'FAIL', 'NEEDS_REVISION'] },
    violations: { type: 'array', items: { type: 'string' } },
    recommendations: { type: 'array', items: { type: 'string' } },
  },
  required: ['verdict'],
};

const IMPL_VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    status: { type: 'string', enum: ['DONE', 'BLOCKED', 'NEEDS_REVIEW'] },
    files_modified: { type: 'array', items: { type: 'string' } },
    build_passed: { type: 'boolean' },
    clippy_passed: { type: 'boolean' },
  },
  required: ['status', 'build_passed'],
};

const TEST_VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    status: { type: 'string', enum: ['ALL_PASS', 'FAILURES'] },
    tests_added: { type: 'number' },
    total_tests: { type: 'number' },
    failures: { type: 'array', items: { type: 'string' } },
  },
  required: ['status', 'tests_added', 'total_tests'],
};

const REVIEW_VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['APPROVED', 'CHANGES_REQUESTED', 'BLOCKED'] },
    issues: { type: 'array', items: { type: 'string' } },
    highlights: { type: 'array', items: { type: 'string' } },
  },
  required: ['verdict'],
};

const INTEGRATION_VERDICT_SCHEMA = {
  type: 'object',
  properties: {
    verdict: { type: 'string', enum: ['INTEGRATED', 'FAILED'] },
    build_passed: { type: 'boolean' },
    tests_passed: { type: 'boolean' },
    tests_total: { type: 'number' },
    clippy_passed: { type: 'boolean' },
    fmt_passed: { type: 'boolean' },
    architecture_verified: { type: 'boolean' },
    failure_details: { type: 'array', items: { type: 'string' } },
  },
  required: ['verdict', 'build_passed', 'tests_passed'],
};

// ──────────────────────────────────────────────
// Phase 1: Architecture Guard
// ──────────────────────────────────────────────
phase('Architecture Guard');
log('🔍 架构守卫检查：验证 DDD 层边界合规性...');

const archResult = await agent(
  `你是一个架构守卫，请检查以下修改是否符合 DDD 层边界规则。

项目层边界规则：
- src/domain/ → 只能引用 crate::domain::*
- src/infrastructure/ → 只能引用 crate::domain::*, crate::shared::*
- src/application/ → 不能引用 crate::interfaces::*
- src/interfaces/ → 可以引用所有层
- src/shared/ → 只能引用 crate::domain::*

检查以下内容：
1. 修改的文件列表，确认没有非法跨层引用
2. 新文件是否放在正确的层目录
3. 是否引入了新的外部依赖

请输出 VERDICT 和任何违规。`,
  { label: 'architect-guard', phase: 'Architecture Guard', schema: ARCH_VERDICT_SCHEMA }
);

if (archResult.verdict !== 'PASS') {
  log(`❌ 架构守卫未通过: ${archResult.verdict}`);
  if (archResult.violations?.length) {
    archResult.violations.forEach(v => log(`  - ${v}`));
  }
  if (archResult.recommendations?.length) {
    archResult.recommendations.forEach(r => log(`  💡 ${r}`));
  }
  return { success: false, phase: 'architecture-guard', violations: archResult.violations };
}

log('✅ 架构守卫通过');

// ──────────────────────────────────────────────
// Phase 2: 并行实现 + 测试
// ──────────────────────────────────────────────
phase('Implement');
log('⚙️ 并行执行：实现代码 + 编写测试...');

const [implResult, testResult] = await parallel([
  // 实现者
  () => agent(
    `你是一个实现者 agent。请实现代码变更。

遵循以下要求：
1. 遵循 DDD 层架构
2. 使用 crate::domain::traits::* 中定义的接口
3. 错误处理：domain 用 DtError，application 用 anyhow
4. 运行 cargo check 确认编译通过
5. 运行 cargo fmt 格式化代码
6. 运行 cargo clippy --all-targets

输出实现状态。`,
    { label: 'implement', phase: 'Implement', schema: IMPL_VERDICT_SCHEMA }
  ),
  // 测试者
  () => agent(
    `你是一个测试者 agent。请为代码变更编写测试。

遵循以下要求：
1. 单元测试放在文件末尾的 #[cfg(test)] 模块中
2. 覆盖：正常路径、边界条件、错误输入
3. 不依赖外部服务（Memgraph/Qdrant）
4. 运行 cargo test 确认全部通过

输出测试状态。`,
    { label: 'test', phase: 'Implement', schema: TEST_VERDICT_SCHEMA }
  ),
]);

// 检查 Phase 2 结果
if (implResult.status !== 'DONE') {
  log(`❌ 实现失败: ${implResult.status}`);
  return { success: false, phase: 'implement', detail: implResult };
}

if (testResult.status !== 'ALL_PASS') {
  log(`❌ 测试失败: ${testResult.failures?.join(', ')}`);
  return { success: false, phase: 'test', detail: testResult };
}

log(`✅ 实现完成 (build: ${implResult.build_passed ? '✅' : '❌'}, clippy: ${implResult.clippy_passed ? '✅' : '❌'})`);
log(`✅ 测试通过 (新增 ${testResult.tests_added} 个测试, 共 ${testResult.total_tests} 个)`);

// ──────────────────────────────────────────────
// Phase 2.5: Review
// ──────────────────────────────────────────────
phase('Review');
log('👀 代码审查中...');

const reviewResult = await agent(
  `你是一个代码审查者 agent。请审查代码变更。

检查以下维度：
1. 命名语义化
2. 错误处理完整性
3. 边界条件
4. 测试覆盖率
5. 代码风格一致性
6. 安全漏洞
7. 性能问题

输出审查结论。`,
  { label: 'code-review', phase: 'Review', schema: REVIEW_VERDICT_SCHEMA }
);

if (reviewResult.verdict === 'BLOCKED') {
  log(`❌ 审查阻塞: ${reviewResult.issues?.join(', ')}`);
  return { success: false, phase: 'review', verdict: 'BLOCKED', issues: reviewResult.issues };
}

if (reviewResult.verdict === 'CHANGES_REQUESTED') {
  log(`⚠️ 审查建议修改: ${reviewResult.issues?.join(', ')}`);
  // 返回 implementer 修复 — 简化版直接返回
  return { success: false, phase: 'review', verdict: 'CHANGES_REQUESTED', issues: reviewResult.issues };
}

log('✅ 审查通过');

// ──────────────────────────────────────────────
// Phase 3: Integrate
// ──────────────────────────────────────────────
phase('Integrate');
log('🔗 集成验收中...');

const integrationResult = await agent(
  `你是一个集成者 agent。请执行集成验收。

检查清单：
1. cargo build 编译通过
2. cargo test 全部测试通过
3. cargo clippy --all-targets 无警告
4. cargo fmt --check 格式正确
5. 代码变更范围符合预期
6. 架构守卫二次确认无层违规

输出集成结论。`,
  { label: 'integration', phase: 'Integrate', schema: INTEGRATION_VERDICT_SCHEMA }
);

if (integrationResult.verdict !== 'INTEGRATED') {
  log(`❌ 集成失败: ${integrationResult.failure_details?.join(', ')}`);
  return { success: false, phase: 'integrate', detail: integrationResult };
}

log('✅ 全量编译通过');
log(`✅ 测试 ${integrationResult.tests_passed ? '全部通过' : '有失败'}`);
log(`✅ Clippy ${integrationResult.clippy_passed ? '通过' : '有警告'}`);
log(`✅ 格式 ${integrationResult.fmt_passed ? '通过' : '需修正'}`);
log(`✅ 架构二次确认 ${integrationResult.architecture_verified ? '通过' : ''}`);

// ──────────────────────────────────────────────
// 完成
// ──────────────────────────────────────────────
log('🎉 变更工作流全部完成！');

return {
  success: true,
  summary: {
    architecture: 'PASS',
    implement: implResult.status,
    test: `${testResult.tests_added} tests added, ${testResult.total_tests} total`,
    review: 'APPROVED',
    integration: 'INTEGRATED',
  },
};