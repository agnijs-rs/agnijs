//! Engine micro-benchmarks.
//!
//! The same JavaScript workloads run on every engine behind the engine
//! interface (the owned engine and the QuickJS-NG adapter), so their times
//! and memory can be compared side by side and against earlier runs. This is
//! a rough, early harness for catching large gaps and regressions while the
//! engine grows; it is not a statistically rigorous benchmark suite.
//!
//! - [`workloads`]: the registry of workloads and selection by filter.
//! - [`stats`]: summaries of timing samples.
//! - [`results`]: the machine-readable results file.
//! - [`compare`]: comparing two results files.
//! - [`runner`]: running one workload on one engine.

pub mod compare;
pub mod machine;
pub mod memory;
pub mod results;
pub mod runner;
pub mod stats;
pub mod workloads;
