//! Machine details recorded with every run.

use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub os: String,
    pub arch: String,
    /// CPU model name, best effort ("unknown" when the platform hides it).
    pub cpu: String,
    pub logical_cpus: usize,
    /// `rustc -V` of the toolchain on PATH, when it runs.
    pub rustc: Option<String>,
    /// `release` or `debug`: the profile the harness was built with.
    pub profile: String,
}

impl Machine {
    pub fn detect() -> Machine {
        Machine {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpu: cpu_model().unwrap_or_else(|| "unknown".into()),
            logical_cpus: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
            rustc: command_line("rustc", &["-V"]),
            profile: if cfg!(debug_assertions) { "debug" } else { "release" }.into(),
        }
    }
}

fn command_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "windows") {
        // The registry holds the marketing name; PROCESSOR_IDENTIFIER is
        // only the family/model/stepping.
        let out = command_line(
            "reg",
            &["query", r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0", "/v", "ProcessorNameString"],
        );
        out.and_then(|t| t.lines().find_map(|l| l.split("REG_SZ").nth(1).map(|s| s.trim().to_string())))
            .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
    } else if cfg!(target_os = "macos") {
        command_line("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else {
        let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        info.lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().to_string())
    }
}
