//! The binary the size report measures (docs/binary-size.md): one realm,
//! one script taken from the command line, its completion value printed.
//! The script is not known at build time, so every built-in stays
//! reachable and the linker can drop only what no script could use.
//!
//! Build it the way the report does, e.g.
//! `cargo build --profile size -p agnijs-js-vm --example size_probe`.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::Engine;
use agnijs_js_vm::Vm;

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    match vm.eval_script(realm, "probe.js", &src) {
        Ok(v) => println!("{v:?}"),
        Err(e) => println!("error: {e}"),
    }
}
