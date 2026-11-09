//! The machine-readable results file.

use crate::machine::Machine;
use crate::stats::Summary;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Format tag of the results file; bumped when a field changes meaning.
pub const SCHEMA: &str = "engine-bench-1";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResultFile {
    pub schema: String,
    /// Seconds since the Unix epoch when the run started.
    pub created_unix: u64,
    /// Free-form label, for example a commit id.
    pub label: Option<String>,
    pub machine: Machine,
    pub config: Config,
    pub results: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub scale: f64,
    pub warmup: usize,
    pub iterations: usize,
}

/// One workload on one engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub workload: String,
    pub category: String,
    pub engine: String,
    pub samples_ns: Vec<u64>,
    pub summary: Summary,
    pub checksum: String,
    /// Engine-reported heap after the last iteration (engine-specific
    /// accounting, not comparable across engines byte for byte).
    pub heap_bytes: usize,
    pub object_count: usize,
    /// Peak resident set (working set on Windows) of the process that ran
    /// only this workload on only this engine; `None` when the platform
    /// gives no figure.
    pub peak_rss_bytes: Option<u64>,
}

impl ResultFile {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("results serialise")
    }

    pub fn from_json(text: &str) -> Result<ResultFile> {
        // Check the tag first so an older or newer file reports its schema
        // rather than whichever field changed.
        let raw: serde_json::Value = serde_json::from_str(text)?;
        let schema = raw.get("schema").and_then(|s| s.as_str()).unwrap_or("<none>");
        if schema != SCHEMA {
            bail!("results file has schema {schema}, this harness reads {SCHEMA}");
        }
        Ok(serde_json::from_value(raw)?)
    }

    pub fn entry(&self, workload: &str, engine: &str) -> Option<&Entry> {
        self.results.iter().find(|e| e.workload == workload && e.engine == engine)
    }

    /// Median time of `engine` over median time of `baseline` on one
    /// workload: above 1 means `engine` is slower.
    pub fn engine_ratio(&self, workload: &str, engine: &str, baseline: &str) -> Option<f64> {
        let a = self.entry(workload, engine)?.summary.median_ns as f64;
        let b = self.entry(workload, baseline)?.summary.median_ns as f64;
        (b > 0.0).then(|| a / b)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn entry(workload: &str, engine: &str, median_ns: u64) -> Entry {
        Entry {
            workload: workload.into(),
            category: "loops".into(),
            engine: engine.into(),
            samples_ns: vec![median_ns],
            summary: Summary { min_ns: median_ns, median_ns, mean_ns: median_ns, max_ns: median_ns },
            checksum: "42".into(),
            heap_bytes: 1024,
            object_count: 10,
            peak_rss_bytes: Some(4096),
        }
    }

    pub(crate) fn file(results: Vec<Entry>) -> ResultFile {
        ResultFile {
            schema: SCHEMA.into(),
            created_unix: 1_700_000_000,
            label: Some("test".into()),
            machine: Machine {
                os: "windows".into(),
                arch: "x86_64".into(),
                cpu: "Test CPU".into(),
                logical_cpus: 8,
                rustc: Some("rustc 1.90.0".into()),
                profile: "release".into(),
            },
            config: Config { scale: 1.0, warmup: 1, iterations: 5 },
            results,
        }
    }

    #[test]
    fn a_results_file_round_trips_through_json() {
        let f = file(vec![entry("loop-arith", "gable", 300), entry("loop-arith", "quickjs", 100)]);
        assert_eq!(ResultFile::from_json(&f.to_json()).unwrap(), f);
    }

    #[test]
    fn a_file_with_another_schema_is_rejected() {
        let mut f = file(vec![]);
        f.schema = "engine-bench-0".into();
        let err = ResultFile::from_json(&f.to_json()).unwrap_err().to_string();
        assert!(err.contains("engine-bench-0"), "{err}");
    }

    #[test]
    fn engine_ratio_divides_medians() {
        let f = file(vec![entry("loop-arith", "gable", 300), entry("loop-arith", "quickjs", 100)]);
        assert_eq!(f.engine_ratio("loop-arith", "gable", "quickjs"), Some(3.0));
        assert_eq!(f.engine_ratio("loop-arith", "quickjs", "gable"), Some(1.0 / 3.0));
        assert_eq!(f.engine_ratio("regex", "gable", "quickjs"), None);
    }
}
