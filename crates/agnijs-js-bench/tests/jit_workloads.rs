//! Every workload computes the same checksum interpreted and with every
//! eligible block compiled (TASK-101.3). Built with `--features jit`; the
//! mode comes from `AGNIJS_JIT`, which the engine reads when it is made, so
//! this file holds one test that sets it between runs.
#![cfg(feature = "jit")]

use agnijs_js_bench::runner::{run, EngineKind, RunConfig};
use agnijs_js_bench::workloads::registry;

#[test]
fn every_workload_computes_the_same_checksum_under_the_jit() {
    let cfg = RunConfig { scale: 0.01, warmup: 0, iterations: 2 };
    let mut mismatches = Vec::new();
    for w in registry() {
        let mut sums = Vec::new();
        for mode in ["off", "force", "on"] {
            std::env::set_var("AGNIJS_JIT", mode);
            let m = run(EngineKind::Owned, &w, &cfg).unwrap_or_else(|e| panic!("{} with AGNIJS_JIT={mode}: {e:#}", w.name));
            sums.push((mode, m.checksum));
        }
        if sums.iter().any(|(_, c)| *c != sums[0].1) {
            mismatches.push(format!("{}: {sums:?}", w.name));
        }
    }
    std::env::remove_var("AGNIJS_JIT");
    assert!(mismatches.is_empty(), "checksums differ:\n{}", mismatches.join("\n"));
}
