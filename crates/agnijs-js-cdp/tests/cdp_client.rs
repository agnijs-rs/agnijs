//! A DevTools-protocol client over a real WebSocket: discovery through
//! `/json/list`, then console messages, a source-mapped uncaught exception,
//! breakpoints, stepping and evaluation against the engine.

use agnijs_engine_api::conformance::TestHost;
use agnijs_js_cdp::{ws, CdpServer, Session};
use serde_json::{json, Value as Json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::Duration;

const CALC: &str = "function add(a, b) {
  const sum = a + b;
  console.log('sum', sum);
  return sum;
}
globalThis.total = add(2, 3);
";

/// Generated code whose source map (made with source-map-js's
/// SourceMapGenerator) points the `throw` at src/app.ts 5:5 and the call
/// at 10:1 (1-based, as stack traces print them).
const APP: &str = "function fail(msg) {
  throw new Error(msg);
}
fail('boom');
//# sourceMappingURL=data:application/json;charset=utf-8;base64,eyJ2ZXJzaW9uIjozLCJzb3VyY2VzIjpbInNyYy9hcHAudHMiXSwibmFtZXMiOlsiZmFpbCJdLCJtYXBwaW5ncyI6IkFBRUE7RUFFSTs7QUFLSkEiLCJmaWxlIjoiYXBwLmpzIn0=
";

struct Client {
    stream: TcpStream,
    next_id: u64,
    events: VecDeque<Json>,
}

fn http_get(addr: SocketAddr, path: &str) -> String {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out.split_once("\r\n\r\n").unwrap().1.to_string()
}

impl Client {
    fn connect(addr: SocketAddr) -> Client {
        let list: Json = serde_json::from_str(&http_get(addr, "/json/list")).unwrap();
        let url = list[0]["webSocketDebuggerUrl"].as_str().unwrap().to_string();
        let path = url.split_once(&addr.to_string()).unwrap().1.to_string();
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        assert!(status.starts_with("HTTP/1.1 101"), "{status}");
        let mut accept = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Sec-WebSocket-Accept:") {
                accept = Some(v.trim().to_string());
            }
        }
        // RFC 6455's worked example key.
        assert_eq!(accept.as_deref(), Some("s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
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
        ws::write_frame(&mut self.stream, ws::OP_TEXT, msg.as_bytes(), Some([7, 1, 9, 3])).unwrap();
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

    fn close(mut self) {
        let _ = ws::write_frame(&mut self.stream, ws::OP_CLOSE, &1000u16.to_be_bytes(), Some([1, 2, 3, 4]));
    }
}

fn top_location(paused: &Json) -> (String, u64, u64) {
    let f = &paused["callFrames"][0];
    (
        f["functionName"].as_str().unwrap().to_string(),
        f["location"]["lineNumber"].as_u64().unwrap(),
        f["location"]["columnNumber"].as_u64().unwrap(),
    )
}

fn client_script(addr: SocketAddr) {
    let mut c = Client::connect(addr);
    c.call("Runtime.enable", json!({}));
    c.call("Debugger.enable", json!({}));
    let bp = c.call(
        "Debugger.setBreakpointByUrl",
        json!({"url": "calc.js", "lineNumber": 1}),
    );
    assert!(bp["breakpointId"].is_string());
    c.call("Runtime.runIfWaitingForDebugger", json!({}));

    // calc.js: scriptParsed, then the breakpoint inside add().
    let parsed = c.event("Debugger.scriptParsed");
    assert_eq!(parsed["url"], "calc.js");
    let calc_id = parsed["scriptId"].as_str().unwrap().to_string();
    let paused = c.event("Debugger.paused");
    assert_eq!(top_location(&paused), ("add".into(), 1, 2));
    assert_eq!(paused["hitBreakpoints"], json!([bp["breakpointId"]]));
    let local = &paused["callFrames"][0]["scopeChain"][0];
    assert_eq!(local["type"], "local");
    let vars = c.call(
        "Runtime.getProperties",
        json!({"objectId": local["object"]["objectId"], "ownProperties": true}),
    );
    let names: Vec<&str> = vars["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"a") && names.contains(&"b"), "{names:?}");
    let r = c.call(
        "Debugger.evaluateOnCallFrame",
        json!({"callFrameId": paused["callFrames"][0]["callFrameId"], "expression": "a * 10 + b"}),
    );
    assert_eq!(r["result"], json!({"type": "number", "value": 23.0, "description": "23"}));
    let src = c.call("Debugger.getScriptSource", json!({"scriptId": calc_id}));
    assert_eq!(src["scriptSource"], CALC);

    // Step over the console.log line: the console event comes first.
    c.call("Debugger.stepOver", json!({}));
    c.event("Debugger.resumed");
    assert_eq!(top_location(&c.event("Debugger.paused")), ("add".into(), 2, 2));
    c.call("Debugger.stepOver", json!({}));
    let console = c.event("Runtime.consoleAPICalled");
    assert_eq!(console["type"], "log");
    assert_eq!(console["args"][0], json!({"type": "string", "value": "sum"}));
    assert_eq!(console["args"][1]["value"], json!(5.0));
    assert_eq!(top_location(&c.event("Debugger.paused")), ("add".into(), 3, 2));
    c.call("Debugger.resume", json!({}));

    // app.js throws: the exception's stack is reported in original positions.
    let parsed = c.event("Debugger.scriptParsed");
    assert_eq!(parsed["url"], "app.js");
    assert!(parsed["sourceMapURL"].as_str().unwrap().starts_with("data:application/json"));
    let thrown = c.event("Runtime.exceptionThrown");
    let details = &thrown["exceptionDetails"];
    assert_eq!(details["text"], "Uncaught");
    assert_eq!(details["url"], "app.js");
    assert_eq!(
        details["exception"]["description"],
        "Error: boom\n    at fail (src/app.ts:5:5)\n    at <anonymous> (src/app.ts:10:1)"
    );
    assert_eq!(details["exception"]["className"], "Error");
    let frames = details["stackTrace"]["callFrames"].as_array().unwrap();
    assert_eq!((frames[0]["lineNumber"].as_u64(), frames[0]["columnNumber"].as_u64()), (Some(1), Some(2)));

    // Back at top level: evaluate in the global scope.
    let r = c.call("Runtime.evaluate", json!({"expression": "total * 2"}));
    assert_eq!(r["result"]["value"], json!(10.0));
    let r = c.call("Runtime.evaluate", json!({"expression": "({x: [1, 'two']})", "returnByValue": true}));
    assert_eq!(r["result"]["value"], json!({"x": [1, "two"]}));
    let r = c.call("Runtime.evaluate", json!({"expression": "missing"}));
    assert_eq!(r["exceptionDetails"]["exception"]["className"], "ReferenceError");
    let r = c.call("Runtime.evaluate", json!({"expression": "[1, 2, 3]"}));
    assert_eq!(r["result"]["subtype"], "array");
    assert_eq!(r["result"]["description"], "Array(3)");
    let length = c.call(
        "Runtime.callFunctionOn",
        json!({"objectId": r["result"]["objectId"], "functionDeclaration": "function (k) { return this.length * k; }", "arguments": [{"value": 4}]}),
    );
    assert_eq!(length["result"]["value"], json!(12.0));
    c.close();
}

#[test]
fn a_devtools_client_debugs_over_a_websocket() {
    let server = CdpServer::listen("127.0.0.1:0", "test").unwrap();
    let addr = server.local_addr();
    let version: Json = serde_json::from_str(&http_get(addr, "/json/version")).unwrap();
    assert_eq!(version["Protocol-Version"], "1.3");
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    let session = Session::new(server, realm);
    session.attach(&mut *d);
    let client = thread::spawn(move || client_script(addr));
    assert!(session.wait_until_ready(&mut *d));
    d.engine().eval_script(realm, "calc.js", CALC).unwrap();
    assert!(d.engine().eval_script(realm, "app.js", APP).is_err());
    session.serve(&mut *d);
    client.join().unwrap();
}

#[test]
fn debugger_pause_interrupts_a_running_script() {
    let server = CdpServer::listen("127.0.0.1:0", "test").unwrap();
    let addr = server.local_addr();
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    let session = Session::new(server, realm);
    session.attach(&mut *d);
    let client = thread::spawn(move || {
        let mut c = Client::connect(addr);
        c.call("Debugger.enable", json!({}));
        c.call("Runtime.runIfWaitingForDebugger", json!({}));
        c.event("Debugger.scriptParsed");
        // The script spins until a variable changes; only a pause can stop it.
        thread::sleep(Duration::from_millis(100));
        c.call("Debugger.pause", json!({}));
        let paused = c.event("Debugger.paused");
        assert_eq!(paused["reason"], "debugCommand");
        assert_eq!(paused["callFrames"][0]["url"], "spin.js");
        c.call(
            "Debugger.evaluateOnCallFrame",
            json!({"callFrameId": "0", "expression": "spinning = false"}),
        );
        c.call("Debugger.resume", json!({}));
        c.close();
    });
    assert!(session.wait_until_ready(&mut *d));
    d.engine()
        .eval_script(realm, "spin.js", "var spinning = true;\nwhile (spinning) {}\n")
        .unwrap();
    session.serve(&mut *d);
    client.join().unwrap();
}

/// A map for `gen.js` placing it in `src/orig.ts`: generated 1:0 -> 1:0,
/// 1:18 -> 10:3 and 2:0 -> 20:0 (all 0-based).
const HOST_MAP: &str = r#"{"version":3,"sources":["orig.ts"],"sourceRoot":"src","names":[],"mappings":"AAAA,kBASG;AAUH"}"#;

#[test]
fn a_host_supplied_source_map_reaches_the_client_and_maps_exceptions() {
    let server = CdpServer::listen("127.0.0.1:0", "test").unwrap();
    let addr = server.local_addr();
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    let session = Session::new(server, realm);
    session.attach(&mut *d);
    let client = thread::spawn(move || {
        let mut c = Client::connect(addr);
        c.call("Runtime.enable", json!({}));
        c.call("Debugger.enable", json!({}));
        c.call("Runtime.runIfWaitingForDebugger", json!({}));
        let parsed = c.event("Debugger.scriptParsed");
        assert_eq!(parsed["url"], "gen.js");
        let url = parsed["sourceMapURL"].as_str().unwrap();
        let b64 = url.strip_prefix("data:application/json;base64,").unwrap();
        // The client gets the host's map text back unchanged.
        let expected = ws::base64_encode(HOST_MAP.as_bytes());
        assert_eq!(b64, expected);
        let thrown = c.event("Runtime.exceptionThrown");
        assert_eq!(
            thrown["exceptionDetails"]["exception"]["description"],
            "Error: m\n    at boom (src/orig.ts:10:4)\n    at <anonymous> (src/orig.ts:20:1)"
        );
        c.close();
    });
    assert!(session.wait_until_ready(&mut *d));
    d.engine().set_source_map("gen.js", Some(HOST_MAP)).unwrap();
    assert!(d
        .engine()
        .eval_script(realm, "gen.js", "function boom() { throw new Error('m') }\nboom();\n")
        .is_err());
    session.serve(&mut *d);
    client.join().unwrap();
}
