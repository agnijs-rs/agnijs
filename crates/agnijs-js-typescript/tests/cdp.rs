//! A DevTools client debugs transpiled TypeScript the way DevTools does:
//! it reads the map from `Debugger.scriptParsed`, sets the breakpoint on
//! the generated position of an original `.ts` line, and maps the pause
//! back to that line.

use agnijs_engine_api::{Host, ImportAttribute, ModuleSource, PromiseState, PropertyKey, Value};
use agnijs_js_cdp::{ws, CdpServer, Session};
use agnijs_js_debugger::source_map::SourceMap;
use agnijs_js_typescript::{transpile, Options};
use serde_json::{json, Value as Json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::rc::Rc;
use std::thread;
use std::time::Duration;

const NAME: &str = "/app/geo.ts";

/// Line 5 (1-based) is the breakpoint's; the enum and the parameter
/// default move it in the output.
const GEO: &str = "interface Point { x: number; y: number }\n\
enum Axis { X, Y }\n\
export function pick(p: Point, axis: Axis = Axis.X): number {\n\
\x20 const a: number = axis === Axis.X ? p.x : p.y;\n\
\x20 return Math.abs(a);\n\
}\n\
export default pick({ x: -3, y: 4 } as Point);\n";

struct TsHost;

impl Host for TsHost {
    fn resolve_module(
        &self,
        _: Option<&str>,
        specifier: &str,
        _: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(specifier.to_string())
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        assert_eq!(resolved, NAME);
        let out = transpile(resolved, GEO, &Options::default()).map_err(|e| e.to_string())?;
        Ok(ModuleSource::SourceWithMap {
            source: out.code,
            source_map: out.source_map,
        })
    }
}

struct Client {
    stream: TcpStream,
    next_id: u64,
    events: VecDeque<Json>,
}

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        let mut s = TcpStream::connect(addr).unwrap();
        write!(s, "GET /json/list HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let list: Json = serde_json::from_str(out.split_once("\r\n\r\n").unwrap().1).unwrap();
        let url = list[0]["webSocketDebuggerUrl"].as_str().unwrap();
        let path = url.split_once(&addr.to_string()).unwrap().1.to_string();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
        }
        Client {
            stream,
            next_id: 0,
            events: VecDeque::new(),
        }
    }

    fn read(&mut self) -> Json {
        let text = ws::read_message(&mut self.stream)
            .expect("read")
            .expect("server closed");
        serde_json::from_str(&text).unwrap()
    }

    fn call(&mut self, method: &str, params: Json) -> Json {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        ws::write_frame(
            &mut self.stream,
            ws::OP_TEXT,
            msg.as_bytes(),
            Some([7, 1, 9, 3]),
        )
        .unwrap();
        loop {
            let m = self.read();
            if m["id"] == json!(id) {
                assert!(m.get("error").is_none(), "{method}: {m}");
                return m["result"].clone();
            }
            self.events.push_back(m);
        }
    }

    fn event(&mut self, method: &str) -> Json {
        if let Some(i) = self.events.iter().position(|e| e["method"] == method) {
            return self.events.remove(i).unwrap()["params"].clone();
        }
        loop {
            let m = self.read();
            if m["method"] == method {
                return m["params"].clone();
            }
            self.events.push_back(m);
        }
    }
}

/// The first generated (line, column), 0-based, that the map places on
/// the original `line`: what DevTools does to set a breakpoint in a
/// source-mapped file.
fn generated_for(map: &SourceMap, generated: &str, line: u32) -> (u32, u32) {
    for (g, text) in (0u32..).zip(generated.lines()) {
        // `lookup` answers with the segment at or before a column, so the
        // first column that answers `line` is where its segment starts.
        for c in 0..text.encode_utf16().count() as u32 {
            if map.lookup(g, c).is_some_and(|o| o.line == line) {
                return (g, c);
            }
        }
    }
    panic!("no generated position for original line {line}");
}

fn client(addr: SocketAddr, generated: String) {
    let mut c = Client::connect(addr);
    c.call("Runtime.enable", json!({}));
    c.call("Debugger.enable", json!({}));
    c.call("Runtime.runIfWaitingForDebugger", json!({}));

    // Paused at the first statement: the module is parsed, its map known.
    let parsed = c.event("Debugger.scriptParsed");
    assert_eq!(parsed["url"], NAME);
    let script_id = parsed["scriptId"].as_str().unwrap().to_string();
    let url = parsed["sourceMapURL"].as_str().unwrap();
    let map = SourceMap::from_data_url(url).unwrap().unwrap();
    assert_eq!(map.sources, [NAME]);
    // The original text travels in the map, so the client can show it.
    assert_eq!(map.sources_content[0].as_deref(), Some(GEO));
    c.event("Debugger.paused");

    let (line, column) = generated_for(&map, &generated, 4);
    assert_ne!(line, 4, "the output should move the line:\n{generated}");
    let bp = c.call(
        "Debugger.setBreakpoint",
        json!({"location": {"scriptId": script_id, "lineNumber": line, "columnNumber": column}}),
    );
    c.call("Debugger.resume", json!({}));

    let paused = c.event("Debugger.paused");
    assert_eq!(paused["hitBreakpoints"], json!([bp["breakpointId"]]));
    let frame = &paused["callFrames"][0];
    assert_eq!(frame["functionName"], "pick");
    let at = &frame["location"];
    let original = map
        .lookup(
            at["lineNumber"].as_u64().unwrap() as u32,
            at["columnNumber"].as_u64().unwrap() as u32,
        )
        .unwrap();
    // `return Math.abs(a);` on line 5, column 3 (1-based).
    assert_eq!(
        (original.source.as_str(), original.line, original.column),
        (NAME, 4, 2)
    );
    let r = c.call(
        "Debugger.evaluateOnCallFrame",
        json!({"callFrameId": frame["callFrameId"], "expression": "a"}),
    );
    assert_eq!(r["result"]["value"], json!(-3.0));
    c.call("Debugger.resume", json!({}));
    let _ = ws::write_frame(
        &mut c.stream,
        ws::OP_CLOSE,
        &1000u16.to_be_bytes(),
        Some([1, 2, 3, 4]),
    );
}

#[test]
fn a_breakpoint_on_a_typescript_line_pauses_there() {
    let generated = transpile(NAME, GEO, &Options::default()).unwrap().code;
    let server = CdpServer::listen("127.0.0.1:0", "test").unwrap();
    let addr = server.local_addr();
    let mut d = agnijs_js_vm::make_debuggee(Rc::new(TsHost));
    let realm = d.engine().main_realm();
    let session = Session::new(server, realm);
    session.attach(&mut *d);
    let client = thread::spawn(move || client(addr, generated));
    assert!(session.wait_until_ready(&mut *d));
    // As `--inspect-brk` does: pause at the program's first statement.
    d.pause_requester().request();
    let p = d.engine().load_module(realm, NAME).unwrap();
    d.engine().run_jobs(realm).unwrap();
    let PromiseState::Fulfilled(ns) = d.engine().promise_state(realm, &p).unwrap() else {
        panic!("the module did not finish");
    };
    assert_eq!(
        d.engine()
            .get(realm, &ns, &PropertyKey::from("default"))
            .unwrap(),
        Value::Number(3.0)
    );
    session.serve(&mut *d);
    client.join().unwrap();
}
