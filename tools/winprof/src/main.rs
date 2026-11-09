//! Minimal out-of-process sampling profiler for Windows. It needs no admin
//! rights, which is why it exists: ETW-based tools (WPR, xperf) do.
//!
//! Usage: winprof <out.folded> <interval_ms> <exe> [args...]
//!
//! It starts `<exe>`, suspends every thread of the child each interval,
//! walks its stack with dbghelp and resumes it. The output is one folded
//! stack per line (`root;...;leaf count`), with inline frames expanded and a
//! trailing `@dir/file.rs:line` pseudo-frame for the leaf address.
//!
//! Set `WINPROF_SYMPATH` to the directories holding the child's PDBs, for
//! example `native\engine\target\release\deps;native\engine\target\release`.
//! Idle threads are sampled too; `analyze.mjs` drops their stacks.
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Diagnostics::Debug::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_AMD64;
use windows_sys::Win32::System::Threading::*;

#[repr(C, align(16))]
struct AlignedCtx(CONTEXT);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: winprof <out.folded> <interval_ms> <exe> [args...]");
        std::process::exit(2);
    }
    let out = args[1].clone();
    let interval: u64 = args[2].parse().expect("interval_ms must be a number");
    let mut child = std::process::Command::new(&args[3])
        .args(&args[4..])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("spawn the child");
    let hproc = child.as_raw_handle() as HANDLE;
    let pid = child.id();
    let sympath = wide(&std::env::var("WINPROF_SYMPATH").unwrap_or_default());
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS | SYMOPT_LOAD_LINES | SYMOPT_LOAD_ANYTHING);
        std::thread::sleep(Duration::from_millis(30));
        if SymInitializeW(hproc, sympath.as_ptr(), 1) == 0 {
            eprintln!("SymInitialize failed: {}", GetLastError());
        }
    }
    let mut stacks: HashMap<Vec<u64>, u64> = HashMap::new();
    let mut threads: HashMap<u32, HANDLE> = HashMap::new();
    let mut modules = HashSet::new();
    let mut names: HashMap<(u64, bool), Vec<String>> = HashMap::new();
    let mut tick = 0u64;
    let mut total = 0u64;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            break;
        }
        if tick % 50 == 0 {
            unsafe { refresh(hproc, pid, &mut modules, &mut threads) };
        }
        tick += 1;
        for h in threads.values() {
            let Some(frames) = (unsafe { sample(hproc, *h) }) else { continue };
            for (i, &addr) in frames.iter().enumerate() {
                // Symbolize while the module is still loaded in the child.
                let leaf = i == 0;
                names.entry((addr, leaf)).or_insert_with(|| symbolize(hproc, addr, leaf));
            }
            *stacks.entry(frames).or_insert(0) += 1;
            total += 1;
        }
        std::thread::sleep(Duration::from_millis(interval));
    }
    eprintln!("samples: {total}, unique stacks: {}", stacks.len());
    let mut f = std::io::BufWriter::new(std::fs::File::create(&out).expect("create the output file"));
    for (stack, n) in &stacks {
        let mut parts: Vec<String> = Vec::new();
        for (i, &addr) in stack.iter().enumerate().rev() {
            let leaf = i == 0;
            let syms = names.entry((addr, leaf)).or_insert_with(|| symbolize(hproc, addr, leaf));
            parts.extend(syms.iter().cloned());
        }
        writeln!(f, "{} {}", parts.join(";"), n).expect("write the output file");
    }
}

/// Load symbols for new modules and open handles to new threads.
unsafe fn refresh(hproc: HANDLE, pid: u32, modules: &mut HashSet<u64>, threads: &mut HashMap<u32, HANDLE>) {
    unsafe {
        SymRefreshModuleList(hproc);
        let ms = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, pid);
        let mut me: MODULEENTRY32W = std::mem::zeroed();
        me.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
        if Module32FirstW(ms, &mut me) != 0 {
            loop {
                let base = me.modBaseAddr as u64;
                if modules.insert(base) {
                    SymLoadModuleExW(hproc, std::ptr::null_mut(), me.szExePath.as_ptr(), std::ptr::null(), base, me.modBaseSize, std::ptr::null(), 0);
                }
                if Module32NextW(ms, &mut me) == 0 {
                    break;
                }
            }
        }
        CloseHandle(ms);
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        let mut te: THREADENTRY32 = std::mem::zeroed();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid && !threads.contains_key(&te.th32ThreadID) {
                    let h = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, 0, te.th32ThreadID);
                    if !h.is_null() {
                        threads.insert(te.th32ThreadID, h);
                    }
                }
                if Thread32Next(snap, &mut te) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
}

/// Suspend one thread, walk its stack (leaf first) and resume it.
unsafe fn sample(hproc: HANDLE, h: HANDLE) -> Option<Vec<u64>> {
    unsafe {
        if SuspendThread(h) == u32::MAX {
            return None;
        }
        let mut ctx: AlignedCtx = std::mem::zeroed();
        ctx.0.ContextFlags = CONTEXT_FULL_AMD64;
        if GetThreadContext(h, &mut ctx.0) == 0 {
            ResumeThread(h);
            return None;
        }
        let mut sf: STACKFRAME64 = std::mem::zeroed();
        sf.AddrPC.Offset = ctx.0.Rip;
        sf.AddrPC.Mode = AddrModeFlat;
        sf.AddrFrame.Offset = ctx.0.Rbp;
        sf.AddrFrame.Mode = AddrModeFlat;
        sf.AddrStack.Offset = ctx.0.Rsp;
        sf.AddrStack.Mode = AddrModeFlat;
        let mut frames = Vec::with_capacity(64);
        for _ in 0..256 {
            let ok = StackWalk64(
                IMAGE_FILE_MACHINE_AMD64 as u32,
                hproc,
                h,
                &mut sf,
                &mut ctx.0 as *mut _ as *mut _,
                None,
                Some(SymFunctionTableAccess64),
                Some(SymGetModuleBase64),
                None,
            );
            if ok == 0 || sf.AddrPC.Offset == 0 {
                break;
            }
            frames.push(sf.AddrPC.Offset);
        }
        ResumeThread(h);
        Some(frames)
    }
}

fn sym_name(hproc: HANDLE, addr: u64, inline_ctx: Option<u32>) -> Option<String> {
    unsafe {
        let mut buf = vec![0u8; std::mem::size_of::<SYMBOL_INFOW>() + 2048];
        let si = buf.as_mut_ptr() as *mut SYMBOL_INFOW;
        (*si).SizeOfStruct = std::mem::size_of::<SYMBOL_INFOW>() as u32;
        (*si).MaxNameLen = 1000;
        let mut disp = 0u64;
        let ok = match inline_ctx {
            Some(c) => SymFromInlineContextW(hproc, addr, c, &mut disp, si),
            None => SymFromAddrW(hproc, addr, &mut disp, si),
        };
        if ok == 0 {
            return None;
        }
        let name = std::slice::from_raw_parts((*si).Name.as_ptr(), (*si).NameLen as usize);
        Some(String::from_utf16_lossy(name).replace(';', ":").replace(' ', ""))
    }
}

/// Names for one return address, outermost first: the real function, then
/// its inlined callees, then a `@dir/file:line` pseudo-frame.
fn symbolize(hproc: HANDLE, addr: u64, leaf: bool) -> Vec<String> {
    // A return address points after the call; step back into it.
    let addr = if leaf { addr } else { addr - 1 };
    unsafe {
        let mut out = Vec::new();
        let n = SymAddrIncludeInlineTrace(hproc, addr);
        if n > 0 {
            let mut ctx = 0u32;
            let mut idx = 0u32;
            if SymQueryInlineTrace(hproc, addr, 0, addr, addr, &mut ctx, &mut idx) != 0 {
                for i in 0..n {
                    if let Some(s) = sym_name(hproc, addr, Some(ctx + i)) {
                        out.push(s);
                    }
                }
            }
        }
        out.push(sym_name(hproc, addr, None).unwrap_or_else(|| format!("0x{addr:x}")));
        out.reverse();
        let mut line: IMAGEHLP_LINEW64 = std::mem::zeroed();
        line.SizeOfStruct = std::mem::size_of::<IMAGEHLP_LINEW64>() as u32;
        let mut d = 0u32;
        if SymGetLineFromAddrW64(hproc, addr, &mut d, &mut line) != 0 && !line.FileName.is_null() {
            let mut n = 0;
            while *line.FileName.add(n) != 0 {
                n += 1;
            }
            let fname = String::from_utf16_lossy(std::slice::from_raw_parts(line.FileName, n));
            let tail: Vec<&str> = fname.rsplit(['\\', '/']).take(2).collect();
            out.push(format!("@{}/{}:{}", tail.get(1).unwrap_or(&""), tail[0], line.LineNumber));
        }
        out
    }
}
