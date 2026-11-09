//! Every workload, shrunk to a tiny scale, runs on every engine and computes
//! the same checksum, so the benchmark compares identical work. This is a
//! correctness check of the workloads, not a benchmark: no timing is
//! asserted.

use agnijs_js_bench::runner::{run, EngineKind, RunConfig};
use agnijs_js_bench::workloads::registry;

#[test]
fn every_workload_computes_the_same_checksum_on_every_engine() {
    let cfg = RunConfig { scale: 0.01, warmup: 0, iterations: 2 };
    let mut mismatches = Vec::new();
    for w in registry() {
        let mut sums = Vec::new();
        for engine in EngineKind::ALL {
            let m = run(engine, &w, &cfg).unwrap_or_else(|e| panic!("{} on {}: {e:#}", w.name, engine.name()));
            assert_eq!(m.samples_ns.len(), 2, "{} on {}", w.name, engine.name());
            assert!(!m.checksum.is_empty() && m.checksum != "undefined", "{} on {}: checksum {:?}", w.name, engine.name(), m.checksum);
            sums.push((engine.name(), m.checksum));
        }
        if sums.iter().any(|(_, c)| *c != sums[0].1) {
            mismatches.push(format!("{}: {sums:?}", w.name));
        }
    }
    assert!(mismatches.is_empty(), "checksums differ:\n{}", mismatches.join("\n"));
}

#[test]
fn a_promise_workload_reports_the_work_done_in_jobs() {
    // The checksum of a job workload is read after the host drains the job
    // queue; at scale 0.01 the chain is 200 links and Promise.all sees 1000.
    let w = registry().into_iter().find(|w| w.name == "promise-chain").unwrap();
    let cfg = RunConfig { scale: 0.01, warmup: 0, iterations: 1 };
    for engine in EngineKind::ALL {
        assert_eq!(run(engine, &w, &cfg).unwrap().checksum, "1200", "{}", engine.name());
    }
}
