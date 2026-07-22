//! Test report — structured check results with coloured terminal output.
//!
//! Each verification step produces a [`CheckResult`] and the overall
//! [`TestReport`] aggregates them for display.

use std::fmt;

// ---------------------------------------------------------------------------
// CheckResult
// ---------------------------------------------------------------------------

/// The outcome of a single verification check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Human-readable check name (e.g. "Classes exist").
    pub name: String,
    /// Category grouping for display: "Graph", "Vector", "Pipeline".
    pub category: String,
    /// Whether the check passed.
    pub passed: bool,
    /// Whether the check was skipped (e.g. because a service was unavailable).
    pub skipped: bool,
    /// Expected outcome description.
    pub expected: String,
    /// Actual outcome description.
    pub actual: String,
}

impl CheckResult {
    /// Create a new passing check result.
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

    /// Create a new failing check result.
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

    /// Create a new skipped check result.
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
            write!(f, "\n      expected: {}", self.expected)?;
            write!(f, "\n      actual:   {}", self.actual)?;
        } else if self.skipped {
            write!(f, "\n      reason: {}", self.actual)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TestReport
// ---------------------------------------------------------------------------

/// Aggregated test report containing all verification checks.
#[derive(Debug)]
pub struct TestReport {
    /// Individual check results.
    pub checks: Vec<CheckResult>,
    /// Total number of checks.
    pub total: usize,
    /// Number of passed checks.
    pub passed: usize,
    /// Number of failed checks.
    pub failed: usize,
    /// Number of skipped checks.
    pub skipped: usize,
    /// Total test duration in milliseconds.
    pub duration_ms: u64,
}

impl TestReport {
    /// Create a new empty report.
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

    /// Add a check result and update counters.
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

    /// Set the duration after all checks complete.
    pub fn set_duration(&mut self, duration_ms: u64) {
        self.duration_ms = duration_ms;
    }

    /// Print a coloured terminal report.
    ///
    /// Uses ANSI colour codes:
    /// - Green `✓` for pass
    /// - Red `✗` for fail
    /// - Yellow `-` for skip
    pub fn print(&self) {
        println!();
        println!("{}", "=".repeat(60));
        println!("  Digital Twin Build Pipeline Test Report");
        println!("{}", "=".repeat(60));
        println!();

        // Group checks by category.
        let mut categories: Vec<&str> = self
            .checks
            .iter()
            .map(|c| c.category.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        categories.sort();

        for cat in &categories {
            let cat_checks: Vec<&CheckResult> = self
                .checks
                .iter()
                .filter(|c| c.category == *cat)
                .collect();

            println!("  [{}] ({})", cat, cat_checks.len());

            for check in &cat_checks {
                let icon = if check.skipped {
                    "\x1b[33m-\x1b[0m"   // Yellow
                } else if check.passed {
                    "\x1b[32m✓\x1b[0m"   // Green
                } else {
                    "\x1b[31m✗\x1b[0m"   // Red
                };

                println!("    {} {}", icon, check.name);

                if !check.passed && !check.skipped {
                    println!("      \x1b[2mexpected: {}\x1b[0m", check.expected);
                    println!("      \x1b[2mactual:   {}\x1b[0m", check.actual);
                } else if check.skipped {
                    println!("      \x1b[33mreason: {}\x1b[0m", check.actual);
                }
            }

            println!();
        }

        // Summary line.
        println!("{}", "-".repeat(60));
        println!(
            "  {} total  |  \x1b[32m{} passed\x1b[0m  |  \x1b[31m{} failed\x1b[0m  |  \x1b[33m{} skipped\x1b[0m  |  {} ms",
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
// Tests
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
        r.add(CheckResult::failed("test-1", "Graph", "count > 0", "count = 0"));
        assert_eq!(r.total, 1);
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
    }

    #[test]
    fn add_skipped_check() {
        let mut r = TestReport::new();
        r.add(CheckResult::skipped("test-1", "Vector", "service unavailable"));
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
