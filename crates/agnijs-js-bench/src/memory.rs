//! Peak memory of the current process.
//!
//! The runner gives every (engine, workload) pair its own process, so the
//! process peak is the pair's peak, covering both the Rust heap of the owned
//! engine and the C heap of QuickJS-NG. It includes the harness binary and
//! both engines' static data, the same for every pair; `engine-startup`
//! shows that floor.

/// Peak resident set size in bytes (peak working set on Windows), or `None`
/// on platforms where it is not read.
pub fn peak_rss_bytes() -> Option<u64> {
    imp::peak_rss_bytes()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    pub fn peak_rss_bytes() -> Option<u64> {
        let mut counters: PROCESS_MEMORY_COUNTERS = Default::default();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        counters.cb = size;
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `counters` is a live, writable PROCESS_MEMORY_COUNTERS
        // whose size is passed as `cb`, as the API requires.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        (ok != 0).then_some(counters.PeakWorkingSetSize as u64)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    pub fn peak_rss_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    // `struct rusage` on 64-bit macOS: two `timeval`s (16 bytes each), then
    // 14 `long`s starting with `ru_maxrss` (bytes on macOS).
    #[repr(C)]
    struct Rusage {
        times: [i64; 4],
        longs: [i64; 14],
    }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    const RUSAGE_SELF: i32 = 0;

    pub fn peak_rss_bytes() -> Option<u64> {
        let mut usage = Rusage { times: [0; 4], longs: [0; 14] };
        // SAFETY: `usage` is a live, writable buffer with the layout and size
        // of `struct rusage` on 64-bit macOS.
        let rc = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
        (rc == 0).then_some(usage.longs[0] as u64)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    pub fn peak_rss_bytes() -> Option<u64> {
        None
    }
}
