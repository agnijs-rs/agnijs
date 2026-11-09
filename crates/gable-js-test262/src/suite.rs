//! Test discovery, selection, parallel execution, the per-directory report
//! and the baseline comparison.

use crate::exclusions::Exclusions;
use crate::features::FeatureCut;
use crate::metadata;
use crate::runner::{self, HarnessFiles, Mode, Outcome, RunResult, TestCase};
use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Why a discovered test was not run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Skip {
    /// Listed in the exclusions file.
    Excluded(String),
    /// Uses a feature outside the ES2025 cut.
    Feature(String),
    /// In a directory the run does not cover (staging, intl402 unless asked).
    OutOfScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestReport {
    pub path: String,
    pub runs: Vec<RunReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunReport {
    pub strict: Option<bool>,
    pub pass: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl From<RunResult> for RunReport {
    fn from(r: RunResult) -> Self {
        match r.outcome {
            Outcome::Pass => RunReport {
                strict: r.strict,
                pass: true,
                message: None,
            },
            Outcome::Fail(m) => RunReport {
                strict: r.strict,
                pass: false,
                message: Some(m),
            },
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DirTotals {
    pub tests: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub engine: String,
    pub test262_commit: String,
    pub totals: DirTotals,
    /// Totals by top-level directory under `test/`.
    pub directories: BTreeMap<String, DirTotals>,
    pub skipped: BTreeMap<String, Skip>,
    pub results: Vec<TestReport>,
}

impl Report {
    /// Paths of tests with at least one failing run, sorted.
    pub fn failing_paths(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .results
            .iter()
            .filter(|t| t.runs.iter().any(|r| !r.pass))
            .map(|t| t.path.clone())
            .collect();
        v.sort();
        v
    }
}

/// The checked-in baseline: the tests expected to fail. Each entry has a
/// path and the failure message observed when the baseline was written.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub engine: String,
    pub test262_commit: String,
    pub expected_failures: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct BaselineDiff {
    /// Tests failing now that the baseline did not expect.
    pub regressions: Vec<String>,
    /// Tests the baseline expected to fail that pass now.
    pub fixed: Vec<String>,
}

impl Baseline {
    pub fn from_report(report: &Report) -> Baseline {
        let mut expected_failures = BTreeMap::new();
        for t in &report.results {
            if let Some(r) = t.runs.iter().find(|r| !r.pass) {
                expected_failures.insert(t.path.clone(), r.message.clone().unwrap_or_default());
            }
        }
        Baseline {
            engine: report.engine.clone(),
            test262_commit: report.test262_commit.clone(),
            expected_failures,
        }
    }

    pub fn compare(&self, report: &Report) -> BaselineDiff {
        let failing = report.failing_paths();
        let regressions = failing
            .iter()
            .filter(|p| !self.expected_failures.contains_key(*p))
            .cloned()
            .collect();
        let ran: std::collections::BTreeSet<&str> =
            report.results.iter().map(|t| t.path.as_str()).collect();
        let fixed = self
            .expected_failures
            .keys()
            .filter(|p| ran.contains(p.as_str()) && !failing.contains(p))
            .cloned()
            .collect();
        BaselineDiff { regressions, fixed }
    }
}

pub struct Selection<'a> {
    pub cut: &'a FeatureCut,
    pub exclusions: &'a Exclusions,
    /// Path prefixes (relative to the root, forward slashes). Empty means
    /// the default set: built-ins, language, annexB, harness.
    pub filters: Vec<String>,
    pub include_intl402: bool,
}

pub fn default_dirs(include_intl402: bool) -> Vec<&'static str> {
    let mut v = vec![
        "test/built-ins/",
        "test/language/",
        "test/annexB/",
        "test/harness/",
    ];
    if include_intl402 {
        v.push("test/intl402/");
    }
    v
}

pub fn load_harness(root: &Path) -> Result<HarnessFiles> {
    let dir = root.join("harness");
    let mut files = HarnessFiles::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("js") {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            files.insert(name, std::fs::read_to_string(&path)?);
        }
    }
    Ok(files)
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Walk `test/` and decide what to run. Fixtures (`_FIXTURE`) are never
/// tests. Returns the runnable cases and the skipped paths with reasons.
pub fn discover(root: &Path, sel: &Selection) -> Result<(Vec<TestCase>, BTreeMap<String, Skip>)> {
    let mut cases = Vec::new();
    let mut skipped = BTreeMap::new();
    let scope: Vec<String> = if sel.filters.is_empty() {
        default_dirs(sel.include_intl402)
            .into_iter()
            .map(String::from)
            .collect()
    } else {
        sel.filters.clone()
    };
    for entry in walkdir::WalkDir::new(root.join("test")).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy();
        if !name.ends_with(".js") || name.contains("_FIXTURE") {
            continue;
        }
        let rel = rel_path(root, path);
        if !scope
            .iter()
            .any(|p| rel.starts_with(p.as_str()) || rel == *p)
        {
            continue;
        }
        let source =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let meta = metadata::parse(&source).map_err(|e| anyhow::anyhow!("{rel}: {e}"))?;
        if let Some(reason) = sel.exclusions.reason_for(&rel, &meta) {
            skipped.insert(rel, Skip::Excluded(reason.to_string()));
            continue;
        }
        if let Err(feature) = sel.cut.check(&meta.features) {
            skipped.insert(rel, Skip::Feature(feature.to_string()));
            continue;
        }
        cases.push(TestCase {
            path: path.to_path_buf(),
            rel,
            meta,
            source,
        });
    }
    Ok((cases, skipped))
}

pub struct RunOptions {
    pub timeout: Duration,
    pub threads: usize,
    pub engine_name: String,
    pub test262_commit: String,
}

/// Run every case, one fresh engine per run, in parallel.
pub fn run_all(
    cases: &[TestCase],
    skipped: BTreeMap<String, Skip>,
    mode: Mode,
    harness: &HarnessFiles,
    opts: &RunOptions,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Report> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads)
        .stack_size(64 * 1024 * 1024)
        .build()?;
    let total = cases.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let mut results: Vec<TestReport> = pool.install(|| {
        cases
            .par_iter()
            .map(|case| {
                let runs = runner::run_test(case, &mode, harness, opts.timeout);
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(n, total);
                TestReport {
                    path: case.rel.clone(),
                    runs: runs.into_iter().map(RunReport::from).collect(),
                }
            })
            .collect()
    });
    results.sort_by(|a, b| a.path.cmp(&b.path));

    let mut directories: BTreeMap<String, DirTotals> = BTreeMap::new();
    let mut totals = DirTotals::default();
    for t in &results {
        let d = directories.entry(top_dir(&t.path)).or_default();
        let failed = t.runs.iter().any(|r| !r.pass);
        d.tests += 1;
        totals.tests += 1;
        if failed {
            d.failed += 1;
            totals.failed += 1;
        } else {
            d.passed += 1;
            totals.passed += 1;
        }
    }
    for p in skipped.keys() {
        directories.entry(top_dir(p)).or_default().skipped += 1;
        totals.skipped += 1;
    }
    Ok(Report {
        engine: opts.engine_name.clone(),
        test262_commit: opts.test262_commit.clone(),
        totals,
        directories,
        skipped,
        results,
    })
}

fn top_dir(rel: &str) -> String {
    // "test/built-ins/Array/x.js" -> "built-ins"
    rel.split('/').nth(1).unwrap_or("").to_string()
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn fixture_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test262");
    root.join("harness/assert.js").exists().then_some(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(paths: &[(&str, bool)]) -> Report {
        Report {
            engine: "x".into(),
            test262_commit: "c".into(),
            totals: DirTotals::default(),
            directories: BTreeMap::new(),
            skipped: BTreeMap::new(),
            results: paths
                .iter()
                .map(|(p, pass)| TestReport {
                    path: p.to_string(),
                    runs: vec![RunReport {
                        strict: None,
                        pass: *pass,
                        message: (!pass).then(|| "boom".into()),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn baseline_diff_reports_regressions_and_fixes() {
        let base = Baseline::from_report(&report(&[("test/a.js", false), ("test/b.js", true)]));
        assert_eq!(
            base.expected_failures.get("test/a.js").map(String::as_str),
            Some("boom")
        );
        let diff = base.compare(&report(&[("test/a.js", true), ("test/b.js", false)]));
        assert_eq!(diff.regressions, vec!["test/b.js".to_string()]);
        assert_eq!(diff.fixed, vec!["test/a.js".to_string()]);
    }

    #[test]
    fn top_dir_is_the_suite_directory() {
        assert_eq!(top_dir("test/built-ins/Array/x.js"), "built-ins");
    }
}
