//! Comparing two results files.
//!
//! Pairs are matched by (workload, engine). The ratio is the new median over
//! the old one, so 1.25 means 25% slower. Changes within the threshold are
//! reported as unchanged: these are micro-benchmarks on a desktop machine
//! and run-to-run noise of 5 to 10% is normal.

use crate::results::ResultFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Faster,
    Slower,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub workload: String,
    pub engine: String,
    pub old_median_ns: u64,
    pub new_median_ns: u64,
    pub ratio: f64,
    pub verdict: Verdict,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Comparison {
    /// Pairs present in both files, in the new file's order.
    pub changes: Vec<Change>,
    /// `workload/engine` pairs only in the old file.
    pub only_old: Vec<String>,
    /// `workload/engine` pairs only in the new file.
    pub only_new: Vec<String>,
}

impl Comparison {
    pub fn regressions(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| c.verdict == Verdict::Slower)
    }
}

/// Compare `new` against `old`; `threshold` is the fractional change that
/// counts as real (0.10 is 10%).
pub fn compare(old: &ResultFile, new: &ResultFile, threshold: f64) -> Comparison {
    let key = |w: &str, e: &str| format!("{w}/{e}");
    let mut cmp = Comparison::default();
    for n in &new.results {
        let Some(o) = old.entry(&n.workload, &n.engine) else {
            cmp.only_new.push(key(&n.workload, &n.engine));
            continue;
        };
        let (old_ns, new_ns) = (o.summary.median_ns, n.summary.median_ns);
        let ratio = if old_ns == 0 { f64::INFINITY } else { new_ns as f64 / old_ns as f64 };
        let verdict = if ratio > 1.0 + threshold {
            Verdict::Slower
        } else if ratio < 1.0 / (1.0 + threshold) {
            Verdict::Faster
        } else {
            Verdict::Unchanged
        };
        cmp.changes.push(Change {
            workload: n.workload.clone(),
            engine: n.engine.clone(),
            old_median_ns: old_ns,
            new_median_ns: new_ns,
            ratio,
            verdict,
        });
    }
    for o in &old.results {
        if new.entry(&o.workload, &o.engine).is_none() {
            cmp.only_old.push(key(&o.workload, &o.engine));
        }
    }
    cmp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::results::tests::{entry, file};

    #[test]
    fn pairs_are_matched_and_classified_against_the_threshold() {
        let old = file(vec![entry("a", "agnijs", 100), entry("b", "agnijs", 100), entry("c", "agnijs", 100)]);
        let new = file(vec![entry("a", "agnijs", 150), entry("b", "agnijs", 105), entry("c", "agnijs", 50)]);
        let cmp = compare(&old, &new, 0.10);
        let got: Vec<(&str, f64, Verdict)> =
            cmp.changes.iter().map(|c| (c.workload.as_str(), c.ratio, c.verdict)).collect();
        assert_eq!(
            got,
            [("a", 1.5, Verdict::Slower), ("b", 1.05, Verdict::Unchanged), ("c", 0.5, Verdict::Faster)]
        );
        assert_eq!(cmp.regressions().map(|c| c.workload.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn the_same_workload_on_different_engines_is_a_different_pair() {
        let old = file(vec![entry("a", "agnijs", 100), entry("a", "quickjs", 10)]);
        let new = file(vec![entry("a", "quickjs", 20)]);
        let cmp = compare(&old, &new, 0.10);
        assert_eq!(cmp.changes.len(), 1);
        assert_eq!((cmp.changes[0].engine.as_str(), cmp.changes[0].ratio), ("quickjs", 2.0));
        assert_eq!(cmp.only_old, ["a/agnijs"]);
        assert!(cmp.only_new.is_empty());
    }

    #[test]
    fn pairs_only_in_the_new_file_are_listed() {
        let old = file(vec![]);
        let new = file(vec![entry("regex", "agnijs", 100)]);
        let cmp = compare(&old, &new, 0.10);
        assert!(cmp.changes.is_empty());
        assert_eq!(cmp.only_new, ["regex/agnijs"]);
    }
}
