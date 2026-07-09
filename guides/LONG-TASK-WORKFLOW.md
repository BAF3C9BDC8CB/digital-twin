# 长任务工作流：全功能交付 + 记忆系统

> 本文档是 digital-twin 技能的一部分。在开始任何长任务前，必须先加载 digital-twin skill。

---

## 一、总览

```
KG查上下文(含测试账号/端口)
  → Brainstorming(含询问测试环境)
  → Writing Plans(含依赖分析)
  → 逐任务执行[子agent实现→子agent审查→写记忆]
  → 集成阶段(拆Mock接真实)
  → 整体验收 → Push → 会话结束
```

### 核心原则

| 原则 | 含义 |
|------|------|
| **全功能交付** | 每个任务产出完整功能代码 + 全量测试通过，不截断 |
| **依赖感知** | 被依赖任务未完成时用 Mock 替代，记忆记录，集成时替换 |
| **浏览器测试** | 网页功能每任务执行，子 agent 用 Playwright/项目既有工具自测 |
| **三层审查** | 实现子agent → Spec审查子agent → 代码质量审查子agent |
| **主 agent 纯编排** | 只负责发任务、汇总审查结果、写记忆，不写一行代码、不做一次浏览器操作 |
| **内容优先验证** | 浏览器验证优先用 snapshot 分析元素内容，不依赖截图识别 |
| **记忆贯穿** | 每阶段写 Neo4j，跨会话可回溯 |

---

## 二、Phase 0: 会话启动 — 查 KG 上下文

加载 digital-twin skill 后，**静默执行**：

```cypher
// 查项目最近事件
MATCH (e:Event)
WHERE e.project = $project
RETURN e.type, e.details, e.timestamp
ORDER BY e.timestamp DESC LIMIT 10

// 查相关决策
MATCH (k:Knowledge)-[:ABOUT]->(p:Project {name: $project})
RETURN k.title, k.details, k.createdAt
ORDER BY k.createdAt DESC LIMIT 10

// 查当前进度
MATCH (k:Knowledge)
WHERE k.project = $project AND k.entity_type = "IntegrationState"
RETURN k.details

// 查测试账号（关键！）
MATCH (k:Knowledge)
WHERE k.project = $project AND k.entity_type = "TestAccount"
RETURN k.details

// 查项目开发端口配置
MATCH (k:Knowledge)
WHERE k.project = $project AND k.entity_type = "DevEnvironment"
RETURN k.details
```

---

## 三、Phase 1: Brainstorming（需求设计 + 测试环境采集）

加载 **brainstorming** skill，标准流程：

1. 探索项目上下文
2. 逐个提问澄清需求（一次一个问题，优先选择题）

3. **测试环境必问项（在需求澄清过程中采集）：**

   - 前端开发服务器端口号/URL → 记录为 `DevEnvironment`
   - 测试账号用户名和密码 → 记录为 `TestAccount`
   - 如果涉及多角色：问每个角色的测试账号
   - 确认是否有现成的 e2e 测试工具（Playwright/Cypress/无）

4. 提出 2-3 种方案 + 推荐方案及理由
5. 逐模块呈现设计，用户逐模块审批
6. 保存设计文档，commit

```bash
# 记忆写入：决策
dt memorize --type Decision \
  --entity-id "arch-$(date +%Y%m%d)-<功能名>" \
  --entity-type ArchitectureDecision \
  --project "<项目>" \
  --details "decision: <决策内容>; reason: <原因>; scope: <影响范围>"

# 记忆写入：测试环境
dt memorize --type Environment \
  --entity-id "dev-env-$(date +%Y%m%d)" \
  --entity-type DevEnvironment \
  --project "<项目>" \
  --details "{\"dev_url\": \"http://localhost:3000\", \"notes\": \"前端端口 3000, 后端 8080\"}"

# 记忆写入：测试账号
dt memorize --type Credentials \
  --entity-id "test-account-<项目>" \
  --entity-type TestAccount \
  --project "<项目>" \
  --details "{\"role\": \"admin\", \"username\": \"admin@test.com\", \"password\": \"<掩码>\", \"notes\": \"测试环境专属账号\"}"
```

> **密码处理规则:** 密码写入 KG 时做掩码处理（如 `adm***com`），仅记录部分字符用于识别。主 agent 在每次会话中重新询问完整密码，不在知识图谱中明文存储敏感凭据。

---

## 四、Phase 2: Writing Plans（依赖感知的任务分解）

加载 **writing-plans** skill，但**替换默认粒度**为：每个任务 = 一个完整功能单元。

### 4.1 任务粒度

```
❌ 默认超能力粒度（2-5分钟）：
   Task 1: 创建 UserService 接口
   Task 2: 实现 UserService 方法A
   Task 3: 实现 UserService 方法B

✅ 本工作流粒度（完整功能）：
   Task 1: 用户注册功能（表单 → 验证 → 持久化 → 错误提示）
   Task 2: 用户登录功能（认证 → Session → 拦截器 → 页面跳转）
```

### 4.2 依赖标注

每个任务在 plan 中必须标注依赖：

```markdown
### Task 1: 用户注册页面
**依赖:** [] （独立任务）

### Task 2: 用户登录页面
**依赖:** [Task 5: Session 中间件]

### Task 3: 个人信息页面
**依赖:** [Task 1, Task 2]
```

### 4.3 执行排序算法

主 agent 根据依赖关系排序：

```
排序规则:
1. 找出所有入度为0的任务（无依赖）→ 优先执行
2. 执行完成后，更新剩余任务的依赖状态
3. 被依赖任务完成后，依赖它的任务从 Mock 模式进入可集成状态
4. 重复直到所有任务完成

示例:
  任务 [1,2,3,4,5]，依赖: 2→5, 1→5, 4→3
  
  第一轮: Task 3（独立）, Task 5（独立）
  第二轮: Task 4（依赖3已完成）, Task 1 Mock版（Mock Task5）
  第三轮: Task 2 Mock版（Mock Task5,依赖1Mock版）
  
   === 集成阶段 ===
   第四轮: Task 1 拆Mock接Task5，Task 2 拆Mock接Task1
```

### 4.4 文件修改理由

每个计划任务在标注修改的文件时，必须附带**修改理由**：

```markdown
### Task 1: 用户注册页面
**修改文件:**
- `src/pages/Register.vue` — 新增注册表单页面（功能核心）
- `src/api/user.ts` — 新增 register API 调用（接口层）
- `src/router/index.ts` — 添加注册路由（路由配置）
- `tests/Register.spec.ts` — 注册功能的单元测试

**不改的文件:**
- `src/api/auth.ts` — 不涉及认证逻辑
- `src/store/user.ts` — 本次不需要状态管理
```

规则：
- 计划中每个被修改的文件必须有**为什么改它**的理由
- 计划中明确**不改的文件**防止越界修改
- 子 agent 在修改文件前必须确认：这个改动在计划范围内吗？
- 如果发现计划外的文件需要修改，子 agent 应返回 NEEDS_CONTEXT 询问

---

## 五、Phase 3: 逐任务执行（核心）

### 5.0 主 agent 的职责边界

```
主 agent 只做三件事:
├─ 发任务 -> 写 prompt -> 发子 agent
├─ 收结果 -> 阅读审查报告 -> 判断通过/退回
└─ 写记忆 -> dt memorize / dt event

主 agent 坚决不做:
❌ 不写任何代码
❌ 不手动操作浏览器
❌ 不直接运行测试
❌ 不修改文件内容

任何需要"动手"的事，都交给子 agent。
```

### 5.0.1 主 agent 执行流程

```
对每个任务:
  Step 1: 发实现子 agent
    ├─ prompt: 计划文本 + 项目结构 + Mock说明(如有)
    |          + 测试账号信息 + 项目开发URL
    ├─ 子 agent: TDD → 实现 → 单元测试 → 浏览器测试(Web) → commit
    └─ 返回 → 主 agent 收结果

  Step 2: 读子 agent 返回
    ├─ DONE → 继续
    ├─ DONE_WITH_CONCERNS → 读备注，判断是否阻塞
    ├─ NEEDS_CONTEXT → 补充上下文重新发
    └─ BLOCKED → 分析原因，升级或拆任务

  Step 3: 发 Spec 合规审查子 agent
    ├─ prompt: spec原文 + 代码diff
    ├─ 返回: ✅ | ❌
    └─ 主 agent 读结果，有问题 → 回实现子 agent 修复

  Step 4: 发代码质量审查子 agent（Mock 任务跳过）
    ├─ prompt: 代码diff + 测试结果
    ├─ 返回: ✅ | ❌
    └─ 主 agent 读结果，有问题 → 回实现子 agent 修复

  Step 5: 主 agent 写记忆 + 标记 TodoWrite 完成

  Step 6: 若有审查问题 → 循环 Step 1
```

### 5.0.2 实现子 agent 上下文构建规则

主 agent 在构建子 agent prompt 时，必须包含以下上下文：

```yaml
项目上下文:
  - 项目名: ${PROJECT}
  - 项目根路径: ${PROJECT_PATH}
  - 技术栈: ${TECH_STACK}  # 从项目文件推断

开发环境:
  - 前端开发URL: ${DEV_URL}  # 从 KG 的 DevEnvironment 读取
  - 端口: ${PORT}             # 同上

测试账号:
  - 角色: admin
  - 用户名: ${USERNAME}       # 从 KG 的 TestAccount 读取
  - 密码: ${PASSWORD}         # 会话中用户提供
  - 备注: ${NOTES}

测试工具:
  - 项目已有 e2e: ${HAS_PLAYWRIGHT_OR_CYPRESS}  # 从 package.json 推断
  - 安装命令: npm install 或 yarn

当前任务:
  - 任务名: ${TASK_NAME}
  - 是否 Mock: ${IS_MOCK}
  - 依赖的 Mock: ${MOCKED_DEPENDENCIES}
  - 计划内容: ${TASK_PLAN}
```

### 5.1 实现子 agent 详细流程

```
你收到: 计划任务文本 + Mock 说明(如有) + 项目结构 + 测试账号 + 开发URL

1. 阅读任务内容 — 有疑问返回 NEEDS_CONTEXT
2. 否则按以下流程执行:

   ┌───────────────────────────────────────────┐
   │  TDD 循环:                                │
   │  ├─ 写测试（单元测试 + 集成测试）          │
   │  ├─ 跑测试 → 确认失败                     │
   │  ├─ 实现代码                              │
   │  ├─ 跑测试 → 确认全过                     │
   │  └─ 若有 Mock: 标注 MOCK 注释             │
   ├───────────────────────────────────────────┤
   │  浏览器测试（仅 Web 功能）:                │
   │  ├─ 检查项目现有 e2e 工具                  │
   │  │   ├─ Playwright → npx playwright test   │
   │  │   ├─ Cypress → npx cypress run          │
   │  │   ├─ Puppeteer → 编写脚本 node test.js  │
   │  │   └─ 无 → npm install --save-dev playwright  │
   │  ├─ 编写浏览器测试脚本:                    │
   │  │   ├─ 登录（使用提供的测试账号）          │
   │  │   ├─ 导航到测试页面                     │
   │  │   ├─ 执行操作（填表/点击/等待）         │
   │  │   └─ 验证内容（不是截图）               │
   │  │       ├─ ✅ page.textContent() 包含预期  │
   │  │       ├─ ✅ page.$() 元素存在            │
   │  │       ├─ ✅ page.url() 包含预期路径      │
   │  │       └─ ❌ 避免依赖截图对比（不可靠）  │
   │  ├─ 运行浏览器测试脚本                      │
   │  └─ 确认全部通过                           │
   ├───────────────────────────────────────────┤
   │  自审查:                                   │
   │  ├─ 检查是否覆盖 spec 所有需求             │
   │  ├─ 检查是否有额外未要求功能               │
   │  └─ 记录发现的问题                         │
   ├───────────────────────────────────────────┤
   │  commit                                    │
   ├───────────────────────────────────────────┤
   │  返回:                                     │
   │  ├─ STATUS: DONE / DONE_WITH_CONCERNS      │
   │  ├─ COMMIT_SHA                             │
   │  ├─ UNIT_TESTS: 12/12 pass                 │
   │  ├─ BROWSER_TESTS: 5/5 pass (Web功能时)     │
   │  ├─ CONCERNS: 任何遗留问题                  │
   │  └─ EVIDENCE: 内容验证结果摘要              │
   └───────────────────────────────────────────┘
```

### 5.2 Mock 模式详细规格

当一个任务的依赖尚未完成时，实现子 agent 执行 Mock 模式：

```python
# 示例: Task 1 依赖 Task 5 (Session 中间件)
# 版本1 - Mock 实现（Round 1）

# session_manager.py

class SessionManager:
    """
    MOCK IMPLEMENTATION — waits for Task 5
    Memory: TASK_1_MOCK_WAITING_TASK_5
    """

    def get_session(self, user_id: str) -> dict:
        # Mock: 返回固定 session 数据
        return {
            "user_id": user_id,
            "token": "mock-token-xxxxx",
            "expires_at": "2099-12-31",
            "role": "admin",
            "_mock": True  # 标记告诉审查这是 Mock
        }

    def validate_session(self, token: str) -> bool:
        return True  # Mock: 永远通过
```

```python
# 测试也使用 Mock 数据:

def test_login_flow_with_mock_session():
    # 使用 Mock SessionManager 测试登录流程
    sm = SessionManager()
    session = sm.get_session("test-user")
    assert session["_mock"] is True  # 断言 Mock 标记
    # ... 测试实际的登录功能
```

```bash
# 记忆记录
dt memorize --type Dependencies \
  --entity-id "mock-task1-waiting-task5" \
  --entity-type PendingIntegration \
  --project "<项目>" \
  --details "{\"mock_task\": \"task1_user_registration\", \"waiting_for\": \"task5_session_middleware\", \"mock_file\": \"src/auth/session_manager.py\", \"status\": \"pending_integration\"}"
```

**Mock 实现规则：**

| 规则 | 说明 |
|------|------|
| 必须标记 | 代码中有明确 `MOCK` 注释 |
| 必须写记忆 | 记录哪些文件是 Mock、等待谁 |
| 测试必须过 | Mock 版的测试也要全部通过 |
| 跳过代码质量审查 | Mock 代码不做质量审查，集成时补 |
| 不得遗漏 | 集成阶段必须逐个替换 Mock |

### 5.3 浏览器测试详细流程（子 agent 执行）

主 agent 不操作浏览器。浏览器测试由**实现子 agent 自己完成**，使用 Playwright 或项目既有 e2e 工具。

#### 5.3.1 上下文获取

子 agent 从主 agent 的 prompt 中获得：

```
开发URL: http://localhost:3000          # 从 KG DevEnvironment 读
测试账号: admin / adm***com / Pass123   # 从 KG TestAccount + 用户提供
项目技术栈: Vue 3 + Vite                # 从 package.json 推断
```

#### 5.3.2 浏览器测试工具选择策略

子 agent 按优先级选择浏览器测试工具：

```
1. 项目已有 Playwright → npx playwright test
2. 项目已有 Cypress → npx cypress run  
3. 项目已有 Puppeteer → node e2e-test.js
4. 有 npm/node → npm install --save-dev playwright && npx playwright test
5. 都不是 → 使用 bash + curl 做 API 级验证（不测 DOM）
```

如果没有 Playwright，自动安装：

```bash
cd ${PROJECT_PATH}
npm install --save-dev @playwright/test
npx playwright install chromium
```

#### 5.3.3 测试脚本模式（Playwright 示例）

子 agent 编写浏览器测试，**必须包含登录流程**：

```javascript
// e2e/login-and-test.spec.js
const { test, expect } = require('@playwright/test');

const DEV_URL = 'http://localhost:3000';  // 从上下文获取
const TEST_USER = {
  username: 'admin@test.com',            // 从上下文获取
  password: 'Pass123'                    // 从上下文获取
};

test('用户注册 - 正常流程', async ({ page }) => {
  // 1. 登录
  await page.goto(`${DEV_URL}/login`);
  await page.fill('#username', TEST_USER.username);
  await page.fill('#password', TEST_USER.password);
  await page.click('#loginBtn');
  await page.waitForURL('**/dashboard');

  // 2. 导航到注册页面
  await page.goto(`${DEV_URL}/register`);

  // 3. 填写表单
  await page.fill('#email', 'newuser@test.com');
  await page.fill('#name', 'New User');
  await page.click('#submitBtn');

  // 4. 内容验证（优先用文本/元素分析，不要依赖截图）
  await page.waitForSelector('.success-message');
  const message = await page.textContent('.success-message');
  expect(message).toContain('注册成功');

  // ❌ 不要: expect(await page.screenshot()).toMatchSnapshot()
});

test('用户注册 - 必填项验证', async ({ page }) => {
  await page.goto(`${DEV_URL}/login`);
  await page.fill('#username', TEST_USER.username);
  await page.fill('#password', TEST_USER.password);
  await page.click('#loginBtn');
  await page.waitForURL('**/dashboard');

  await page.goto(`${DEV_URL}/register`);
  await page.click('#submitBtn');

  // 验证 HTML5 验证或自定义错误提示 — 用文本内容判断
  const errorText = await page.textContent('.error-message');
  expect(errorText).toBeTruthy();
  expect(errorText.length).toBeGreaterThan(0);
});
```

#### 5.3.4 内容验证优先原则

```
✅ 优先使用的验证方式（按优先级）:
   1. page.textContent() 包含预期文本  — "页面显示了'操作成功'"
   2. page.$() 元素存在                — "提交按钮变灰了"
   3. page.url() 包含预期路径          — "跳转到 /dashboard"
   4. page.$eval() 检查样式/属性       — "错误提示是红色"
   5. console 无报错                    — 检查浏览器控制台

❌ 避免使用的验证方式:
   1. page.screenshot() 像素对比       — 不可靠，不支持图片识别的 agent 无法判断
   2. 截图人工看                       — agent 无法判断截图内容
   3. 视觉回归测试（Visual Regression）— 仅在专门的视觉测试阶段使用
```

#### 5.3.5 登录流程标准化

对于需要登录的功能，测试脚本统一模式：

```javascript
// 登录辅助函数（每个测试文件开头）
async function loginAs(page, role) {
  const credentials = {
    admin: { username: 'admin@test.com', password: 'Pass123' },
    user:  { username: 'user@test.com',  password: 'Pass456' },
  };
  const cred = credentials[role] || credentials.admin;

  await page.goto(`${DEV_URL}/login`);
  await page.fill('#username', cred.username);
  await page.fill('#password', cred.password);
  await page.click('#loginBtn');
  await page.waitForURL('**/dashboard');
}

// 测试中调用
test('个人信息页面', async ({ page }) => {
  await loginAs(page, 'admin');
  await page.goto(`${DEV_URL}/profile`);
  // ... 测试内容
});
```

#### 5.3.6 端口/URL 自适应

不硬编码端口。子 agent 从上下文获取 `DEV_URL`。如果开发服务器未运行，子 agent 应：

```bash
# 1. 检查端口是否可用
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000

# 2. 如果不可用，尝试启动（根据项目类型）
npm run dev &
# 或
cd server && mvn spring-boot:run &

# 3. 等待服务就绪
while ! curl -s http://localhost:3000 > /dev/null 2>&1; do sleep 2; done
```

#### 5.3.7 浏览器测试结果格式

实现子 agent 返回的浏览器测试结果：

```markdown
## Browser Test Results

**Task:** 用户注册页面

**工具:** Playwright (chromium headless)

| 用例 | 结果 | 验证方式 |
|------|------|----------|
| 正常注册 | ✅ | textContent 包含"注册成功"，URL 跳转到 /login |
| 密码不匹配 | ✅ | textContent 包含"密码不匹配"，错误框红色 |
| 必填项为空 | ✅ | HTML5 validity 触发，表单未提交 |
| 控制台错误 | ✅ 无错误 | console 无 error/warning |

**测试日志摘要:**
```
✓ login-and-test.spec.js:3:1 › 用户注册 - 正常流程
✓ login-and-test.spec.js:3:1 › 用户注册 - 必填项验证
✓ login-and-test.spec.js:3:1 › 用户注册 - 密码不匹配
```

**结论:** 浏览器测试全部通过 ✅（基于 DOM 内容分析，非截图）
```

### 5.4 文件修改理由规范（执行阶段）

子 agent 在修改每个文件前，必须自行确认修改理由：

```
修改文件前的自问:
├─ 这个文件在计划的"修改文件"列表中吗？
│   ├─ 是 → 按计划修改，理由已在计划中说明
│   └─ 否 → 停下来，问自己：
│       ├─ 这个修改真的必要吗？
│       ├─ 是否可以通过修改计划内文件达到相同目的？
│       └─ 如果确实必要 → 返回 NEEDS_CONTEXT 问主 agent
│
├─ 这个修改会引入不需要的副作用吗？
│   ├─ 会破坏其他功能？
│   ├─ 会引入新的依赖？
│   └─ 会改变已有行为？
│
└─ 修改理由是否在 commit message 中体现了？
    feat(Register): 新增注册页面表单
    - src/pages/Register.vue: 新增表单组件（功能入口）
    - src/api/user.ts: 新增 register API（接口对接）
    - src/router/index.ts: 添加 /register 路由（导航配置）
```

如果确实需要修改计划外的文件，子 agent 必须在返回中详细说明：

```
MODIFIED_OUTSIDE_PLAN:
  - src/utils/validation.ts
    理由: 注册和登录都用到相同验证规则，抽取为公用函数（DRY 重构）
    建议: 纳入计划，作为 Task 1 的补充
```

**审查中会检查：** Spec 合规审查子 agent 会核对代码 diff 与计划文件列表的一致性。未经说明的计划外修改会被标记为 `❌ EXTRA_MODIFICATIONS`。

---

## 六、三层审查标准

### 6.1 Spec 合规审查（子 agent）

```markdown
**角色:** Spec 合规审查子 agent
**输入:** spec 原文 + 代码 diff + 测试结果

**检查项:**
1. ✅ 所有 spec 中的功能点是否都被实现？
2. ✅ 是否有 spec 之外的额外功能（YAGNI 违规）？
3. ✅ 测试是否覆盖了 spec 描述的边界条件？
4. ✅ API/组件接口是否与 spec 一致？

**返回格式:**
- ✅ SPEC_COMPLIANT — 完全匹配 spec
- ❌ MISSING_FEATURES — 遗漏了以下功能: [...]
- ❌ EXTRA_FEATURES — 包含了未要求的功能: [...]
- ❌ INTERFACE_MISMATCH — 接口与 spec 不一致: [...]
```

### 6.2 代码质量审查（子 agent）

```markdown
**角色:** 代码质量审查子 agent
**输入:** 代码 diff + 单元测试结果
**跳过条件:** 该任务是 Mock 实现（记忆中有 PendingIntegration 标记）

**检查项:**
1. ✅ 命名语义化（函数/变量/类名）
2. ✅ 错误处理（所有异常路径有处理）
3. ✅ 边界条件（null、空值、越界）
4. ✅ 测试覆盖率（关键路径有测试）
5. ✅ 一致性问题（Task N 和 Task N+1 的接口兼容）
6. ✅ 安全漏洞（SQL 注入、XSS、未授权访问）
7. ✅ 性能问题（N+1 查询、内存泄漏）

**返回格式:**
- ✅ QUALITY_OK — 代码质量合格
- ❌ NAMING_ISSUES — 命名需要改进: [...]
- ❌ ERROR_HANDLING — 遗漏错误处理: [...]
- ❌ SECURITY_ISSUES — 存在安全风险: [...]
- ❌ TEST_COVERAGE — 测试覆盖不足: [...]
```

### 6.3 主 agent 最终确认

```markdown
**确认流程:**
1. 阅读 Spec 审查结果
2. 阅读代码质量审查结果（非 Mock 任务）
3. 阅读浏览器测试结果（Web 任务）
4. 判断:
   - 所有审查通过 → 标记完成
   - 审查有异议（如 Spec 和代码质量审查矛盾）→ 询问人类
   - 审查发现问题 → 通知实现子 agent 修复 → 重新审查
5. 写记忆
```

---

## 七、Phase 4: 集成阶段

Mock 任务积累到一定程度后，启动集成阶段。

### 7.1 集成触发条件

```markdown
集成检查:
1. 被依赖的任务（如 Task 5）已完成且审查通过
2. 依赖它的 Mock 任务（如 Task 1）存在记忆中的 Mock 标记
```

### 7.2 集成执行

```
集成子 agent 收到:
├─ Mock 任务列表 [Task 1, Task 2]
├─ 对应的真实依赖 [Task 5, Task 1]
├─ Mock 文件路径
└─ 项目结构

执行:
1. 逐个打开 Mock 文件
2. 将 Mock 实现替换为真实依赖调用
3. 清理 Mock 标记注释
4. 更新测试（拆 Mock 数据 → 用真实依赖）
5. 跑全量测试
6. 浏览器测试（Web 功能）
7. 更新记忆

记忆更新:
├─ dt event --type TaskReimplemented \
     --entity-id "task1-integrated" \
     --entity-type FeatureTask \
     --details "mocks_resolved: [task5]; integration_status: complete"
└─ dt remove --project <项目> --file src/auth/session_manager.py (if Mock file was separate)
    dt build --file ... (if Mock was inline)
```

---

## 八、Phase 5: 整体验收

所有任务完成 + 集成完成后，执行最终验收。

### 8.1 验收清单

```
□ 全量测试通过
  运行: pytest / npm test / mvn test
  预期: 0 failures

□ 类型检查通过
  运行: npm run typecheck / mypy / tsc --noEmit
  预期: 0 errors

□ 代码风格检查
  运行: npm run lint / ruff check / eslint
  预期: 0 warnings

□ 浏览器全流程测试（子 agent 执行）
  npx playwright test --project=chromium
  验证: 全部测试通过（基于 DOM 内容分析，不可用截图判断）
  检查: 无残留测试账号硬编码

□ 需求逐项核验
  重新读 spec → 逐条确认已实现

□ 无残留 Mock
  搜索代码中的 MOCK 标记 → 确认全部清理

□ 记忆完整性
  确认所有 dt event / dt memorize 已执行

□ 展示完整 diff 给用户确认
  git diff （等待用户确认）
```

### 8.2 Verification Before Completion

遵循 `verification-before-completion` skill 铁律：

```
❌ 不允许:
   "应该可以了"
   "测试大概能过"  
   "之前跑过没问题"
   "我对代码有信心"
   
✅ 必须:
   跑命令 → 读输出 → 确认通过 → 才能说"完成了"
```

---

## 九、Phase 6: 会话结束（每次都写，覆盖旧进度）

### 9.0 触发条件

**每次 AI 向用户汇报/总结之前自动执行，不需要用户说"结束"。** 具体时机：

```
自动触发时机（满足任一即执行）:
├─ 一个完整功能点实现完毕，准备向用户汇报结果 → **先写记忆，再汇报**
├─ 用户验收通过，准备展示 diff → **先写记忆，再展示 diff**
├─ 遇到阻塞，准备告诉用户无法继续 → **先写记忆，再说明阻塞**
├─ 该轮所有任务执行完毕，准备做工作总结 → **先写记忆，再总结**
├─ 即将给出最终回复之前 → **先写记忆，再回复**

核心原则: 在任何"给用户看结果"的动作之前，先把当前进度写到 KG。
            用户看到的结果什么，KG 里的记录就应该是什么。
            下一次会话打开时，不需要问用户"上次做了什么"。
```

每次写入使用**固定 entity-id**，自动覆盖上一次的记录，始终只保留最新进度。

记录的信息必须让**下一次会话能无缝续接**。

### 9.1 续接上下文规范

每次会话结束时，KG 中必须记录以下信息：

```yaml
# 写入 KG 的 details 字段结构
continuation_context:
  # 0. 总进度百分比
  progress_percent: 20            # 当前完成百分比（累计）
  total_tasks: 10                  # 总任务数
  tasks_completed_this_session: 2  # 本次完成的任务数
  session_summary: "完成了注册页面和 Session 中间件的 Mock"  # 本次干了什么

  # 1. 任务状态
  tasks:
    completed: ["Task 1: 注册页面 ✅", "Task 5: Session 中间件 ✅"]
    pending: ["Task 2: 登录页面 (依赖 Task 5, 已 Mock ⏳)", "Task 3: 个人中心"]
    blocked: ["Task 4: 密码重置 (依赖验证码服务, 等待第三方接入)"]
    integration_pending: ["Task 2 → Task 5"]

  # 2. 每个已完成任务的具体描述（谁在什么时候做了什么）
  completed_details:
    - task: "Task 1: 注册页面"
      status: "100% 完成"
      files:
        - "src/pages/Register.vue: 注册表单页面，含表单验证"
        - "src/api/user.ts: register API 对接"
        - "tests/Register.spec.ts: 12 个测试用例"
      browser_test: "通过 (Playwright, 3 用例)"
      commit: "abc1234"
      summary: "用户可填写邮箱/密码/姓名注册，前端验证+API提交"

    - task: "Task 5: Session 中间件 (Mock)"
      status: "Mock 完成，等待集成"
      mock_file: "src/auth/session_manager.py"
      waiting_for: "无依赖，独立"
      summary: "返回固定 session 数据，登录流程已用 Mock 跑通"

  # 3. 环境信息（精确）
  environment:
    dev_url: "http://localhost:3000"
    project_path: "/data/aflmProjects/xxx"
    branch: "feature/user-center"
    test_accounts:
      admin_role: "admin@test.com"
      # 密码不记录，下次会话重新询问

  # 4. 如何继续
  next_steps:
    - "下一件事: 集成 Task 2 (拆 Mock 接 Task 5)"
    - "先跑: cd backend && npm run dev"
    - "注意: 如果 Playwright 未安装，运行 npm install --save-dev @playwright/test"

  # 5. 关键上下文（后续需要的说明/数据/注意事项）
  key_context:
    - "UserService 接口在 src/services/user.ts，Task 5 依赖这个接口"
    - "登录页用了 FormKit 组件库，其他页面也用了"
    - "测试环境数据库每天凌晨 3 点重置"
```

### 9.2 记忆写入命令

使用**固定 entity-id**，每次写入自动覆盖上一次的记录：

```bash
# 1. 列出本次会话做了什么，计算进度百分比

# 2. 写入续接上下文（固定 entity-id，自动覆盖）
PROGRESS_PERCENT=20  # 计算: 已完成任务数 / 总任务数 * 100

dt memorize --type Context \
  --entity-id "continuation-<项目名>" \
  --entity-type SessionContinuation \
  --project "<项目>" \
  --details "{
    \"progress_percent\": $PROGRESS_PERCENT,
    \"total_tasks\": 10,
    \"completed_this_session\": 2,
    \"session_summary\": \"完成了注册页面和 Session 中间件 Mock\",
    \"completed\": [\"Task 1: 注册页面\", \"Task 5: Session 中间件\"],
    \"pending\": [\"Task 2: 登录页面 (Mock Task5)\"],
    \"completed_details\": \"{task: Task 1, files: [Register.vue, api/user.ts, tests], browser_test: pass}\",
    \"next\": \"集成 Task 2: 拆 Mock 接 Task 5\",
    \"dev_url\": \"http://localhost:3000\",
    \"branch\": \"feature/user-center\",
    \"notes\": \"TestAccount 密码已掩码, 下次会话需重新提供\"
  }"

# 3. 记录会话事件（固定 entity-id，同样覆盖）
dt event --type Conversation \
  --entity-id "<项目名>-session" \
  --entity-type Session \
  --project "<项目>" \
  --details "{
    \"date\": \"$(date +%Y-%m-%d)\",
    \"progress\": \"$PROGRESS_PERCENT%\",
    \"completed_this_session\": \"Task 1, Task 5\",
    \"summary\": \"完成了注册页面前端+API，Session 中间件 Mock\"
  }"

# 4. 回复
> 📝 进度 $PROGRESS_PERCENT% 已记录到知识图谱。下次会话自动续接。
```

进度覆盖机制：

```
第一次会话结束:
  → 写入: Knowledge { entity_id: "continuation-项目名", details: { progress: 10%, completed: [Task 1] } }

第二次会话结束:
  → 覆盖: Knowledge { entity_id: "continuation-项目名", details: { progress: 20%, completed: [Task 1, Task 5] } }
  → 旧的 10% 记录被覆盖，KG 中始终只有最新进度

第三次会话结束:
  → 覆盖同上，progress 更新为 30%
```

Phase 0 查询时只取最新的 `SessionContinuation`：

```cypher
// 查续接上下文（只取最新的，使用固定 entity-id）
MATCH (k:Knowledge {entity_id: "continuation-<项目名>"})
RETURN k.details
```

### 9.3 续接时的动作

下次会话启动时（Phase 0 KG 查询），用固定 entity-id 查到最新的 `SessionContinuation`：

```cypher
// 查询最新进度（用固定 entity-id）
MATCH (k:Knowledge {entity_id: "continuation-<项目名>"})
RETURN k.details AS continuation
```

查到后：

1. 向用户展示续接摘要：**"上次进度 20%，完成了 Task 1 和 Task 5，接下来建议做 Task 2 集成"**
2. 询问用户是否继续，或提供新的测试密码
3. 如果继续，直接从 Phase 3 开始执行下一个任务

```
用户: "继续上次的工作"
主 agent:
  ├─ KG查询 → MATCH (k:Knowledge {entity_id: "continuation-<项目名>"})
  ├─ 返回: progress=20%, completed=[Task1, Task5], pending=[Task2]
  ├─ 回复: "上次完成了 20%（Task 1 注册页面 + Task 5 Session Mock），
  │        下一步是集成 Task 2。测试密码和之前一样吗？"
  └─ 用户确认后 → 直接执行集成任务
```

---

## 十、子 agent Prompt 模板

### 10.1 实现子 agent

```markdown
你是一个实现子 agent。

## 上下文
项目: ${PROJECT}
项目路径: ${PROJECT_PATH}
开发URL: ${DEV_URL}
技术栈: ${TECH_STACK}

测试账号:
- 用户名: ${TEST_USERNAME}
- 密码: ${TEST_PASSWORD}
- 角色: ${TEST_ROLE}

当前任务: ${TASK_NAME}
依赖状态: ${DEPENDENCY_STATUS} (如: "Task 5 未完成，使用 Mock")

## 任务计划
${TASK_PLAN_TEXT}

## 要求
1. 阅读计划，有疑问返回 NEEDS_CONTEXT
2. **文件修改必须先确认理由：**
   - 计划中的文件 → 按计划修改
   - 计划外的文件 → 停下来问主 agent（返回 NEEDS_CONTEXT）
   - 不得擅自修改未在计划中列出的文件
3. 按 TDD 流程：写测试 → 确认失败 → 实现 → 确认通过
4. 如果有未完成的依赖，使用 Mock 模式（代码标注 MOCK 注释）
5. 如果该任务是 Mock：不做额外优化，只实现最小可行功能
6. Web 功能：完成 TDD 后，**自己编写并运行浏览器测试**
   - 优先用 Playwright，其次 Cypress/Puppeteer，最差 API 级 curl 测试
   - 测试前确认开发服务器可访问，不可用时自动启动
   - 必须登录（使用提供的测试账号）
   - 验证方式：textContent / 元素存在 / URL 跳转，**不要用截图对比**
7. 自审查：检查 spec 覆盖 + 是否有额外功能
8. commit（每条 commit 写明改了什么文件及理由）

## 返回格式
STATUS: DONE | DONE_WITH_CONCERNS | NEEDS_CONTEXT | BLOCKED
COMMIT_SHA: <git sha>
UNIT_TESTS: <X/Y pass>
BROWSER_TESTS: <X/Y pass> (Web 功能时)
EVIDENCE: <关键验证证据，如 "textContent 包含'操作成功'">
MODIFIED_OUTSIDE_PLAN: <如果有计划外的修改，列出文件+理由>
CONCERNS: <有任何问题写在这里>
```

### 10.2 Spec 审查子 agent

```markdown
你是一个 Spec 合规审查子 agent。

## 输入
- spec 原文（见计划文件）
- 代码 diff（git diff ${SHA}^..${SHA}）

## 检查项
- 所有 spec 功能点已实现？
- 有无额外未要求功能？
- 接口与 spec 一致？
- 测试覆盖 spec 边界条件？
- **所有修改的文件是否都在计划的文件列表中？**
  - 如果有计划外的修改，是否有合理理由说明？
  - 计划内的修改是否理由充分？

## 返回格式
VERDICT: SPEC_COMPLIANT | MISSING_FEATURES | EXTRA_FEATURES | INTERFACE_MISMATCH
DETAILS: <具体说明>
EXTRA_MODIFICATIONS: <如果有计划外的文件修改，列出>
```

### 10.3 代码质量审查子 agent

```markdown
你是一个代码质量审查子 agent。

## 输入
- 代码 diff
- 测试结果

## 检查项
- 命名语义化
- 错误处理完整性
- 边界条件处理
- 测试覆盖率
- 安全漏洞
- 与现有代码风格一致

## 返回格式
VERDICT: QUALITY_OK | ISSUES_FOUND
ISSUES:
  - [严重] <问题描述>
  - [中等] <问题描述>
HIGHLIGHTS: <做得好的地方>
```

---

## 十一、记忆命令速查

| 场景 | 命令 |
|------|------|
| 架构决策 | `dt memorize --type Decision --entity-type ArchitectureDecision --details "decision: ...; reason: ..."` |
| Mock 依赖记录 | `dt memorize --type Dependencies --entity-type PendingIntegration --details "mock_for: <任务>; waiting_for: <依赖>"` |
| Mock 清理记录 | `dt event --type TaskReimplemented --entity-type FeatureTask --details "mocks_resolved: [依赖列表]"` |
| 任务完成 | `dt event --type TaskComplete --entity-type FeatureTask --details "coverage: X%; tests: Y pass"` |
| 代码变更 | `dt build --path <项目> --name <项目>` 或 `dt build --file <文件绝对路径>` |
| 批量同步 | `dt build --path <项目> --name <项目>` |
| 软件安装 | `dt event --type SoftwareInstalled --entity-type Software --details "version: X"` |
| 会话结束 + 续接上下文（覆盖） | `dt memorize --type Context --entity-id "continuation-<项目>" --entity-type SessionContinuation --details "{progress_percent/completed/pending/next/dev_url}"` |
| 会话结束 + 事件（覆盖） | `dt event --type Conversation --entity-id "<项目>-session" --entity-type Session --details "{date/progress/completed/summary}"` |

---

## 十二、完整生命周期示例

以"开发用户中心"为例，展示完整流程：

```
Brainstorming: 确定做注册/登录/个人中心/密码重置 4个功能
→ dt memorize --type Decision --entity-type ArchitectureDecision
→ Writing Plans: 分解为 7 个任务

依赖分析:
  Task 1: 注册页面        dep: []          ← 第一轮
  Task 2: 登录页面        dep: [Task 5]    ← Mock Task5
  Task 3: 个人中心        dep: [Task 1]    ← 第三轮
  Task 4: 密码重置        dep: [Task 5]    ← Mock Task5
  Task 5: Session 中间件  dep: []          ← 第一轮
  Task 6: 验证码服务      dep: []          ← 第一轮
  Task 7: 邮箱通知        dep: [Task 1]    ← 第三轮

执行:
  轮1: Task 1 → 实现+审查+浏览器测试 → ✅ 完成
  轮1: Task 5 → 实现+审查 → ✅ 完成
  轮1: Task 6 → 实现+审查 → ✅ 完成

  轮2: Task 2(Mock Task5) → 实现+Spec审查(跳过质量) → 写记忆 → ⏳ Mock
  轮2: Task 4(Mock Task5) → 实现+Spec审查(跳过质量) → 写记忆 → ⏳ Mock

  轮3: Task 3(依赖Task1完成) → 实现+审查+浏览器测试 → ✅ 完成
  轮3: Task 7(依赖Task1完成) → 实现+审查 → ✅ 完成

集成:
  Task 2: 拆Mock → 接Task5真实 → 全量测试 → 浏览器测试 → 更新记忆 → ✅
  Task 4: 拆Mock → 接Task5真实 → 全量测试 → 浏览器测试 → 更新记忆 → ✅

整体验收:
  全量测试 47/47 pass
  lint 0 errors
  浏览器全流程测试 pass
  需求逐项核验 ✅
  无残留 Mock ✅

展示 diff → 用户确认 → commit → push
dt event --type Conversation → 📝
```
