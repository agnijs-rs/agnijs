//! The size probe (`size_probe`) with the engine-neutral debugger interface
//! in use, for the "debugger attached" row of docs/binary-size.md. The
//! engine is made as a `Debuggee` and, when `AGNIJS_PROBE_DEBUG` is set, a
//! client that logs each pause and continues is attached and a breakpoint
//! is set on the script's first line. Whether to attach is decided at run
//! time, so the whole debugger implementation stays reachable.
//!
//! `cargo build --profile size -p agnijs-js-vm --example size_probe_debugger`

use agnijs_engine_api::conformance::TestHost;
use agnijs_js_debugger::{
    BreakpointSpec, BreakpointTarget, DebugClient, Debuggee, PauseInfo, PauseOnExceptions,
    ResumeAction, ScopeContents,
};
use std::rc::Rc;

struct Logger;

impl DebugClient for Logger {
    fn paused(&self, debuggee: &mut dyn Debuggee, pause: &PauseInfo) -> ResumeAction {
        eprintln!("paused: {pause:?}");
        for frame in debuggee.paused_frames().unwrap_or_default() {
            for scope in &frame.scopes {
                if let ScopeContents::Object(o) = &scope.contents {
                    if let Ok(p) = debuggee.properties(o) {
                        eprintln!("{p:?}");
                    }
                }
            }
        }
        ResumeAction::Continue
    }
}

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    if std::env::var_os("AGNIJS_PROBE_DEBUG").is_some() {
        d.attach(Rc::new(Logger));
        d.set_pause_on_exceptions(PauseOnExceptions::All);
        d.set_breakpoint(BreakpointSpec {
            target: BreakpointTarget::Url("probe.js".into()),
            line: 0,
            column: None,
            condition: None,
        });
    }
    match d.engine().eval_script(realm, "probe.js", &src) {
        Ok(v) => println!("{v:?}"),
        Err(e) => println!("error: {e}"),
    }
}
