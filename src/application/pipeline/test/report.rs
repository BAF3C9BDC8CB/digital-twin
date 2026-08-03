//! 测试报告——带彩色终端输出的结构化检查结果。
//!
//! 每个验证步骤产生一个 [`CheckResult`]，整体 [`TestReport`] 聚合它们用于展示。

use std::fmt;

// ---------------------------------------------------------------------------
// CheckResult
// ---------------------------------------------------------------------------

/// 单个验证检查的结果。
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// 人类可读的检查名称（例如 "Classes exist"）。
    pub name: String,
    /// 用于展示的类别分组："Graph"、"Vector"、"Pipeline"。
    pub category: String,
    /// 检查是否通过。
    pub passed: bool,
    /// 检查是否被跳过（例如因为某个服务不可用）。
    pub skipped: bool,
    /// 预期结果描述。
    pub expected: String,
    /// 实际结果描述。
    pub actual: String,
}

impl CheckResult {
    /// 创建新的通过检查结果。
    pub fn passed(name: impl Into<String>, category: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
            passed: true,
            skipped: false,
            expected: String::new(),
            actual: String::new(),
        }
    }

    /// 创建新的失败检查结果。
    pub fn failed(
        name: impl Into<String>,
        category: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
            passed: false,
            skipped: false,
            expected: expected.into(),
            actual: actual.into(),
        }
    }

    /// 创建新的跳过检查结果。
    pub fn skipped(
        name: impl Into<String>,
        category: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            category: category.into(),
            passed: false,
            skipped: true,
            expected: String::new(),
            actual: reason.into(),
        }
    }
}

impl fmt::Display for CheckResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let icon = if self.skipped {
            "-"
        } else if self.passed {
            "✓"
        } else {
            "✗"
        };

        write!(f, "  {} [{}] {}", icon, self.category, self.name)?;

        if !self.passed && !self.skipped {
            write!(f, "\n      期望: {}", self.expected)?;
            write!(f, "\n      实际:   {}", self.actual)?;
        } else if self.skipped {
            write!(f, "\n      原因: {}", self.actual)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestReport
// ---------------------------------------------------------------------------

/// 包含所有验证检查的聚合测试报告。
#[derive(Debug)]
pub struct TestReport {
    /// 单条检查结果。
    pub checks: Vec<CheckResult>,
    /// 检查总数。
    pub total: usize,
    /// 通过的检查数。
    pub passed: usize,
    /// 失败的检查数。
    pub failed: usize,
    /// 跳过的检查数。
    pub skipped: usize,
    /// 总测试时长（毫秒）。
    pub duration_ms: u64,
}

impl TestReport {
    /// 创建新的空报告。
    pub fn new() -> Self {
        Self {
            checks: Vec::new(),
            total: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            duration_ms: 0,
        }
    }

    /// 添加一条检查结果并更新计数器。
    pub fn add(&mut self, check: CheckResult) {
        if check.skipped {
            self.skipped += 1;
        } else if check.passed {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        self.total += 1;
        self.checks.push(check);
    }

    /// 在所有检查完成后设置时长。
    pub fn set_duration(&mut self, duration_ms: u64) {
        self.duration_ms = duration_ms;
    }

    /// 打印彩色终端报告。
    ///
    /// 使用 ANSI 颜色代码：
    /// - 绿色 `✓` 表示通过
    /// - 红色 `✗` 表示失败
    /// - 黄色 `-` 表示跳过
    pub fn print(&self) {
        println!();
        println!("{}", "=".repeat(60));
        println!("  数字孪生构建流水线测试报告");
        println!("{}", "=".repeat(60));
        println!();

        // 按类别分组检查。
        let mut categories: Vec<&str> = self
            .checks
            .iter()
            .map(|c| c.category.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        categories.sort();

        for cat in &categories {
            let cat_checks: Vec<&CheckResult> =
                self.checks.iter().filter(|c| c.category == *cat).collect();

            println!("  [{}] ({})", cat, cat_checks.len());

            for check in &cat_checks {
                let icon = if check.skipped {
                    "\x1b[33m-\x1b[0m" // 黄色
                } else if check.passed {
                    "\x1b[32m✓\x1b[0m" // 绿色
                } else {
                    "\x1b[31m✗\x1b[0m" // 红色
                };

                println!("    {} {}", icon, check.name);

                if !check.passed && !check.skipped {
                    println!("      \x1b[2m期望: {}\x1b[0m", check.expected);
                    println!("      \x1b[2m实际:   {}\x1b[0m", check.actual);
                } else if check.skipped {
                    println!("      \x1b[33m原因: {}\x1b[0m", check.actual);
                }
            }

            println!();
        }

        // 汇总行。
        println!("{}", "-".repeat(60));
        println!(
            "  {} 总计  |  \x1b[32m{} 通过\x1b[0m  |  \x1b[31m{} 失败\x1b[0m  |  \x1b[33m{} 跳过\x1b[0m  |  {} ms",
            self.total, self.passed, self.failed, self.skipped, self.duration_ms,
        );
        println!("{}", "=".repeat(60));
        println!();
    }
}

impl Default for TestReport {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_report_is_empty() {
        let r = TestReport::new();
        assert_eq!(r.total, 0);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 0);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn add_passed_check() {
        let mut r = TestReport::new();
        r.add(CheckResult::passed("test-1", "Graph"));
        assert_eq!(r.total, 1);
        assert_eq!(r.passed, 1);
        assert_eq!(r.failed, 0);
    }

    #[test]
    fn add_failed_check() {
        let mut r = TestReport::new();
        r.add(CheckResult::failed(
            "test-1",
            "Graph",
            "count > 0",
            "count = 0",
        ));
        assert_eq!(r.total, 1);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
    }

    #[test]
    fn add_skipped_check() {
        let mut r = TestReport::new();
        r.add(CheckResult::skipped(
            "test-1",
            "Vector",
            "service unavailable",
        ));
        assert_eq!(r.total, 1);
        assert_eq!(r.passed, 0);
        assert_eq!(r.skipped, 1);
    }

    #[test]
    fn set_duration() {
        let mut r = TestReport::new();
        r.set_duration(1234);
        assert_eq!(r.duration_ms, 1234);
    }
}
