# Digital Twin Skills

Digital Twin 知识图谱操作规范 - Hermes Agent Skill

## 📦 包含的 Skills

本目录包含 2 个 skill：

| Skill | 说明 | 行数 |
|-------|------|------|
| digital-twin-skill | Digital Twin 完整操作指南（统一版本） | 424 |
| digital-twin-ops | Digital Twin DevOps 操作 | - |

---

## 🚀 快速安装

### 安装（创建软链接）

```bash
# 使用安装脚本
/data/myProject/digital-twin-v2/scripts/install-dt-skills.sh

# 验证安装
/data/myProject/digital-twin-v2/scripts/validate-dt-skills.sh
```

### 手动安装

```bash
cd ~/.hermes/skills/autonomous-ai-agents
ln -sf /data/myProject/digital-twin-v2/skills/digital-twin-skill
```

---

## 📖 使用方式

### 加载 skill

```bash
hermes chat
> skill_view('digital-twin-skill')
```

### skill 包含的内容

**digital-twin-skill** 是一个统一的完整指南，包含：

1. **快速开始** - 健康检查、三个世界、任务路由
2. **代码分析三段序** ⭐⭐⭐ - 环境感知 → KG 定位 → 读码验证
3. **部署与配置管理** - 记忆优先、安全规则
4. **记忆管理** - 三种类型（decision / knowledge / preference）
5. **健康检查与索引** - 状态监控、索引操作
6. **故障排查** - 常见问题解决
7. **五条核心规则** - 快速参考

---

## 🎯 核心工作流（代码分析三段序）

```
① dt_sense()          → 获取项目全貌
    ↓
② dt_search(world=code) → 定位符号位置
    ↓
③ read_file()         → 验证具体实现
```

**红线**: 读代码文件前，① 和 ② 必须完成

---

## 💡 五条核心规则

1. **进项目先 `dt_sense()`** - 获取项目全貌
2. **读码前先 `dt_search(world=code)`** - 定位先于读码
3. **查配置先 `dt_search(world=memory)`** - 记忆优先
4. **用户说"记住"立即 `dt_memorize`** - 不要拖延
5. **永远不要读 `.env` 或输出密钥** - 安全第一

---

## 📁 文件结构

```
skills/
├── digital-twin-skill/
│   └── SKILL.md                      # 统一操作指南
├── digital-twin-ops/
│   └── SKILL.md                      # DevOps 操作
└── README.md                         # 本文件
```

---

## 🔧 维护与更新

### 修改 skill

```bash
# 直接编辑项目中的文件
vim /data/myProject/digital-twin-v2/skills/digital-twin-skill/SKILL.md

# 修改立即生效（软链接）
```

### 卸载

```bash
# 使用卸载脚本
/data/myProject/digital-twin-v2/scripts/uninstall-dt-skills.sh

# 或手动删除
rm ~/.hermes/skills/autonomous-ai-agents/digital-twin-skill
```

---

## 📚 完整文档

- **架构指南**: `/data/myProject/digital-twin-v2/docs/digital-twin-skill-system.md`
- **快速入门**: `/data/myProject/digital-twin-v2/docs/dt-skill-quickstart.md`
- **测试报告**: `/data/myProject/digital-twin-v2/docs/dt-skill-test-report-v3-unified.md`

---

## 🧪 验证与测试

```bash
# 运行验证脚本
/data/myProject/digital-twin-v2/scripts/validate-dt-skills.sh

# 查看 skill 列表
hermes skills list | grep digital-twin

# 测试加载
hermes chat -q 'skill_view("digital-twin-skill")'
```

---

## 📊 统计数据

- **Skill 数量**: 2 个
- **文档行数**: 424 行（digital-twin-skill）
- **软链接**: 1 个
- **测试状态**: ✅ 100% 通过

---

## 🎨 设计特点

- ✅ **统一管理** - 1 个 skill 包含所有功能
- ✅ **按需加载** - 1 次 skill_view() 加载全部
- ✅ **易于维护** - 修改 1 个文件
- ✅ **版本控制友好** - Git 管理源文件

---

**维护者**: Digital Twin Team  
**最后更新**: 2026-09-03  
**版本**: 3.0.0（统一版本）
