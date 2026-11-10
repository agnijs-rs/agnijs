//! `cfg(jit_host)`: the target can generate and run machine code (ADR 0020).
//! Elsewhere (wasm32, iOS, other architectures) the crate builds without a
//! code generator and reports the JIT unavailable. Keep in step with the
//! target table of the Cranelift dependencies in Cargo.toml.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(jit_host)");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch_ok = matches!(arch.as_str(), "x86_64" | "aarch64");
    let os_ok = matches!(os.as_str(), "windows" | "linux" | "macos" | "android");
    if arch_ok && os_ok {
        println!("cargo::rustc-cfg=jit_host");
    }
}
