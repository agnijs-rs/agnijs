//! The size probe (agnijs-js-vm's `size_probe` example) with the debugger
//! and the Chrome DevTools Protocol endpoint linked in, for the "debugger
//! and CDP" row of docs/binary-size.md. The engine is made as a debuggee;
//! when `AGNIJS_PROBE_INSPECT` names an address (`127.0.0.1:9229`) a CDP
//! server listens there and waits for a client before the script runs.
//! Whether to listen is decided at run time, so the whole debugger and
//! the whole adapter stay reachable.
//!
//! `cargo build --profile size -p agnijs-js-cdp --example size_probe_cdp`

use agnijs_engine_api::conformance::TestHost;
use agnijs_js_cdp::{CdpServer, Session};

fn main() {
    let src = std::env::args().nth(1).unwrap_or_default();
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    let mut session = None;
    if let Ok(addr) = std::env::var("AGNIJS_PROBE_INSPECT") {
        match CdpServer::listen(addr.as_str(), "size probe") {
            Ok(server) => {
                eprintln!("{}", server.websocket_url());
                let s = Session::new(server, realm);
                s.attach(&mut *d);
                s.wait_until_ready(&mut *d);
                session = Some(s);
            }
            Err(e) => eprintln!("listen: {e}"),
        }
    }
    match d.engine().eval_script(realm, "probe.js", &src) {
        Ok(v) => println!("{v:?}"),
        Err(e) => println!("error: {e}"),
    }
    if let Some(s) = session {
        s.serve(&mut *d);
    }
}
