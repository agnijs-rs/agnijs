//! agnijs-js-cdp: a Chrome DevTools Protocol adapter.
//!
//! It speaks the `Runtime` and `Debugger` domains on top of the
//! engine-neutral debugger interface ([`agnijs_js_debugger::Debuggee`]), so
//! any engine implementing that interface can be driven by DevTools-protocol
//! clients (Chrome DevTools, VS Code's JavaScript debugger, scripted clients).
//!
//! A [`Session`] owns the protocol state and is the debugger client attached
//! to the engine. It reads messages from a [`Transport`]; [`CdpServer`] is
//! the WebSocket one. Everything runs on the engine's thread: while
//! JavaScript is paused the session blocks in [`DebugClient::paused`],
//! serving messages until the client resumes; otherwise the embedder calls
//! [`Session::poll`] (or [`Session::serve`]) between its own work.
//!
//! Supported: console messages (`Runtime.consoleAPICalled`), uncaught
//! exceptions (`Runtime.exceptionThrown`, with the stack in the exception's
//! description mapped through the script's source map), scripts
//! (`Debugger.scriptParsed`, `getScriptSource`), breakpoints by URL or
//! location with conditions, stepping, pausing on exceptions or on request,
//! async stacks, call frames and scopes, `Runtime.evaluate`,
//! `Debugger.evaluateOnCallFrame`, `Runtime.getProperties` and
//! `Runtime.callFunctionOn`.

// ADR 0019 J10 (TASK-100.32): engine crates stay free of `unsafe`.
#![forbid(unsafe_code)]

pub mod server;
pub mod ws;

pub use server::{CdpOptions, CdpServer};

use agnijs_engine_api::{ConsoleLevel, Error, PropertyKey, RealmId, Value};
use agnijs_js_debugger::{
    AsyncStackTrace, BreakpointId, BreakpointSpec, BreakpointTarget, DebugClient, Debuggee,
    ExceptionEvent, FrameInfo, Location, PauseInfo, PauseOnExceptions, PauseReason,
    PauseRequester, PropertyName, ResumeAction, ScopeContents, ScopeKind, ScriptId, ScriptInfo,
    SourceMap, StackFrameInfo, ValueKind,
};
use serde_json::{json, Value as Json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// What a transport delivered.
pub enum Incoming {
    Message(String),
    /// Nothing waiting (non-blocking reads only).
    Empty,
    /// The client went away; another may connect.
    Disconnected,
    /// The transport is gone for good.
    Closed,
}

/// Carries protocol messages between a client and a [`Session`].
pub trait Transport {
    fn send(&self, message: &str);
    /// The next message; blocks until one arrives when `wait`.
    fn recv(&self, wait: bool) -> Incoming;
    /// Called on attach: a transport that reads on another thread can use
    /// it to act on `Debugger.pause` while JavaScript is running.
    fn set_pause_requester(&self, _requester: PauseRequester) {}
}

/// Fetches a source map that is not a `data:` URL: given the script URL and
/// the `sourceMappingURL`, the map's JSON text.
pub type SourceMapLoader = Box<dyn Fn(&str, &str) -> Option<String>>;

/// The name a `Runtime.evaluate` snippet is compiled under.
const EVALUATE_URL: &str = "";

enum Remote {
    Value(Value),
    Scope(Vec<(String, Value)>),
}

#[derive(Default)]
struct State {
    runtime_enabled: bool,
    debugger_enabled: bool,
    ready: bool,
    paused: bool,
    resume: Option<ResumeAction>,
    objects: HashMap<u64, Remote>,
    groups: HashMap<String, Vec<u64>>,
    next_object: u64,
    breakpoints: HashMap<String, BreakpointId>,
    source_maps: HashMap<ScriptId, Option<Rc<SourceMap>>>,
    next_exception: u32,
    /// A `Runtime.evaluate` is running: its exception is its own result.
    evaluating: u32,
    /// A client has sent a message and not yet disconnected.
    connected: bool,
}

/// A protocol session: the debugger client attached to one engine.
pub struct Session {
    transport: Box<dyn Transport>,
    state: RefCell<State>,
    loader: RefCell<Option<SourceMapLoader>>,
    realm: RealmId,
}

type Reply = Result<Json, (i64, String)>;

fn invalid(message: impl Into<String>) -> (i64, String) {
    (-32602, message.into())
}

fn timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

fn level_name(level: ConsoleLevel) -> &'static str {
    match level {
        ConsoleLevel::Log => "log",
        ConsoleLevel::Info => "info",
        ConsoleLevel::Warn => "warning",
        ConsoleLevel::Error => "error",
        ConsoleLevel::Debug => "debug",
    }
}

fn scope_type(kind: ScopeKind) -> &'static str {
    match kind {
        ScopeKind::Local => "local",
        ScopeKind::Closure => "closure",
        ScopeKind::Catch => "catch",
        ScopeKind::Block => "block",
        ScopeKind::With => "with",
        ScopeKind::Script => "script",
        ScopeKind::Module => "module",
        ScopeKind::Eval => "eval",
        ScopeKind::Global => "global",
    }
}

fn location_json(l: &Location) -> Json {
    json!({
        "scriptId": l.script.0.to_string(),
        "lineNumber": l.line,
        "columnNumber": l.column,
    })
}

fn parse_location(p: &Json) -> Result<Location, (i64, String)> {
    let script = p
        .get("scriptId")
        .and_then(Json::as_str)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| invalid("scriptId"))?;
    Ok(Location {
        script: ScriptId(script),
        line: p.get("lineNumber").and_then(Json::as_u64).ok_or_else(|| invalid("lineNumber"))? as u32,
        column: p.get("columnNumber").and_then(Json::as_u64).unwrap_or(0) as u32,
    })
}

fn stack_frame_json(f: &StackFrameInfo) -> Json {
    json!({
        "functionName": f.function_name,
        "scriptId": f.script.map(|s| s.0.to_string()).unwrap_or_default(),
        "url": f.url,
        "lineNumber": f.line,
        "columnNumber": f.column,
    })
}

fn async_json(trace: &AsyncStackTrace) -> Json {
    let mut v = json!({
        "description": trace.description,
        "callFrames": trace.frames.iter().map(stack_frame_json).collect::<Vec<_>>(),
    });
    if let Some(p) = &trace.parent {
        v["parent"] = async_json(p);
    }
    v
}

fn number_json(n: f64, v: &mut Json) {
    if n.is_nan() {
        v["unserializableValue"] = json!("NaN");
    } else if n.is_infinite() {
        v["unserializableValue"] = json!(if n > 0.0 { "Infinity" } else { "-Infinity" });
    } else if n == 0.0 && n.is_sign_negative() {
        v["unserializableValue"] = json!("-0");
    } else {
        v["value"] = json!(n);
    }
}

impl Session {
    /// A session over `transport` for an engine whose main realm is
    /// `realm` (the realm `Runtime.evaluate` runs in).
    pub fn new(transport: impl Transport + 'static, realm: RealmId) -> Rc<Session> {
        Rc::new(Session {
            transport: Box::new(transport),
            state: RefCell::new(State::default()),
            loader: RefCell::new(None),
            realm,
        })
    }

    /// Attach the session to `debuggee` as its debugger client.
    pub fn attach(self: &Rc<Self>, debuggee: &mut dyn Debuggee) {
        debuggee.attach(self.clone());
        self.transport.set_pause_requester(debuggee.pause_requester());
    }

    /// How to fetch source maps that are not inline `data:` URLs.
    pub fn set_source_map_loader(&self, loader: SourceMapLoader) {
        *self.loader.borrow_mut() = Some(loader);
    }

    /// Handle every message waiting, without blocking. `false` once the
    /// transport has closed.
    pub fn poll(&self, d: &mut dyn Debuggee) -> bool {
        loop {
            match self.transport.recv(false) {
                Incoming::Message(m) => self.handle(d, &m),
                Incoming::Empty => return true,
                Incoming::Disconnected => self.disconnected(d),
                Incoming::Closed => return false,
            }
        }
    }

    /// Block until a client sends `Runtime.runIfWaitingForDebugger`
    /// (the `--inspect-brk` handshake), serving its setup messages.
    /// `false` if the transport closed first.
    pub fn wait_until_ready(&self, d: &mut dyn Debuggee) -> bool {
        while !self.state.borrow().ready {
            match self.transport.recv(true) {
                Incoming::Message(m) => self.handle(d, &m),
                Incoming::Empty => {}
                Incoming::Disconnected => self.disconnected(d),
                Incoming::Closed => return false,
            }
        }
        true
    }

    /// Serve messages while a client is connected: returns when it
    /// disconnects (at once if none is connected) or the transport closes.
    pub fn serve(&self, d: &mut dyn Debuggee) {
        while self.state.borrow().connected {
            match self.transport.recv(true) {
                Incoming::Message(m) => self.handle(d, &m),
                Incoming::Empty => {}
                Incoming::Disconnected => self.disconnected(d),
                Incoming::Closed => return,
            }
        }
    }

    /// Whether a client is connected (has sent a message and not left).
    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }

    fn send(&self, v: Json) {
        self.transport.send(&v.to_string());
    }

    fn event(&self, method: &str, params: Json) {
        self.send(json!({ "method": method, "params": params }));
    }

    /// The client left: drop what it set up, as V8 does for a session.
    fn disconnected(&self, d: &mut dyn Debuggee) {
        let bps: Vec<BreakpointId> = {
            let mut s = self.state.borrow_mut();
            let bps = s.breakpoints.drain().map(|(_, b)| b).collect();
            let paused = s.paused;
            *s = State {
                paused,
                resume: paused.then_some(ResumeAction::Continue),
                ..State::default()
            };
            bps
        };
        for b in bps {
            d.remove_breakpoint(b);
        }
        d.set_pause_on_exceptions(PauseOnExceptions::None);
        d.set_async_stack_depth(0);
        d.set_breakpoints_active(true);
    }

    /// Handle one protocol message.
    pub fn handle(&self, d: &mut dyn Debuggee, message: &str) {
        let Ok(msg) = serde_json::from_str::<Json>(message) else {
            self.send(json!({"error": {"code": -32700, "message": "Message must be valid JSON"}}));
            return;
        };
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        self.state.borrow_mut().connected = true;
        let reply = self.dispatch(d, method, &params);
        match reply {
            Ok(result) => self.send(json!({ "id": id, "result": result })),
            Err((code, message)) => {
                self.send(json!({ "id": id, "error": { "code": code, "message": message } }))
            }
        }
    }

    fn dispatch(&self, d: &mut dyn Debuggee, method: &str, p: &Json) -> Reply {
        let str_param = |name: &str| p.get(name).and_then(Json::as_str);
        match method {
            "Runtime.enable" => {
                self.state.borrow_mut().runtime_enabled = true;
                self.event(
                    "Runtime.executionContextCreated",
                    json!({"context": {
                        "id": self.realm.0 + 1,
                        "origin": "",
                        "name": "main",
                        "uniqueId": format!("agnijs-{}", self.realm.0 + 1),
                        "auxData": {"isDefault": true},
                    }}),
                );
                Ok(json!({}))
            }
            "Runtime.disable" => {
                self.state.borrow_mut().runtime_enabled = false;
                Ok(json!({}))
            }
            "Runtime.runIfWaitingForDebugger" => {
                self.state.borrow_mut().ready = true;
                Ok(json!({}))
            }
            "Runtime.evaluate" => {
                let expr = str_param("expression").ok_or_else(|| invalid("expression"))?;
                let group = str_param("objectGroup").map(str::to_string);
                let by_value = p.get("returnByValue").and_then(Json::as_bool).unwrap_or(false);
                self.state.borrow_mut().evaluating += 1;
                let r = d.engine().eval_script(self.realm, EVALUATE_URL, expr);
                self.state.borrow_mut().evaluating -= 1;
                Ok(self.evaluation_result(d, r, group.as_deref(), by_value))
            }
            "Runtime.getProperties" => self.get_properties(d, p),
            "Runtime.callFunctionOn" => self.call_function_on(d, p),
            "Runtime.releaseObject" => {
                let id = str_param("objectId").ok_or_else(|| invalid("objectId"))?;
                if let Ok(n) = id.parse::<u64>() {
                    self.state.borrow_mut().objects.remove(&n);
                }
                Ok(json!({}))
            }
            "Runtime.releaseObjectGroup" => {
                let group = str_param("objectGroup").ok_or_else(|| invalid("objectGroup"))?;
                self.release_group(group);
                Ok(json!({}))
            }
            "Debugger.enable" => {
                self.state.borrow_mut().debugger_enabled = true;
                for s in d.scripts() {
                    self.script_parsed_event(d, &s);
                }
                Ok(json!({"debuggerId": "agnijs-debugger"}))
            }
            "Debugger.disable" => {
                self.state.borrow_mut().debugger_enabled = false;
                Ok(json!({}))
            }
            "Debugger.getScriptSource" => {
                let id = str_param("scriptId")
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| invalid("scriptId"))?;
                match d.script_source(ScriptId(id)) {
                    Some(src) => Ok(json!({ "scriptSource": src })),
                    None => Err((-32000, "No script for id".into())),
                }
            }
            "Debugger.setBreakpointByUrl" => {
                let line = p
                    .get("lineNumber")
                    .and_then(Json::as_u64)
                    .ok_or_else(|| invalid("lineNumber"))? as u32;
                let Some(url) = str_param("url") else {
                    return Err(invalid("only `url` breakpoints are supported (not urlRegex or scriptHash)"));
                };
                let spec = BreakpointSpec {
                    target: BreakpointTarget::Url(url.to_string()),
                    line,
                    column: p.get("columnNumber").and_then(Json::as_u64).map(|c| c as u32),
                    condition: str_param("condition").filter(|c| !c.is_empty()).map(str::to_string),
                };
                let (id, locations) = d.set_breakpoint(spec);
                let key = id.0.to_string();
                self.state.borrow_mut().breakpoints.insert(key.clone(), id);
                Ok(json!({
                    "breakpointId": key,
                    "locations": locations.iter().map(location_json).collect::<Vec<_>>(),
                }))
            }
            "Debugger.setBreakpoint" => {
                let loc = parse_location(p.get("location").ok_or_else(|| invalid("location"))?)?;
                let spec = BreakpointSpec {
                    target: BreakpointTarget::Script(loc.script),
                    line: loc.line,
                    column: Some(loc.column),
                    condition: str_param("condition").filter(|c| !c.is_empty()).map(str::to_string),
                };
                let (id, locations) = d.set_breakpoint(spec);
                let Some(actual) = locations.first() else {
                    d.remove_breakpoint(id);
                    return Err((-32000, "Could not resolve breakpoint".into()));
                };
                let key = id.0.to_string();
                self.state.borrow_mut().breakpoints.insert(key.clone(), id);
                Ok(json!({ "breakpointId": key, "actualLocation": location_json(actual) }))
            }
            "Debugger.removeBreakpoint" => {
                let key = str_param("breakpointId").ok_or_else(|| invalid("breakpointId"))?;
                let id = self.state.borrow_mut().breakpoints.remove(key);
                if let Some(id) = id {
                    d.remove_breakpoint(id);
                }
                Ok(json!({}))
            }
            "Debugger.getPossibleBreakpoints" => {
                let start = parse_location(p.get("start").ok_or_else(|| invalid("start"))?)?;
                let end = match p.get("end") {
                    Some(e) => Some(parse_location(e)?),
                    None => None,
                };
                let locations = d.breakable_locations(start, end);
                Ok(json!({ "locations": locations.iter().map(location_json).collect::<Vec<_>>() }))
            }
            "Debugger.setBreakpointsActive" => {
                d.set_breakpoints_active(p.get("active").and_then(Json::as_bool).unwrap_or(true));
                Ok(json!({}))
            }
            "Debugger.setPauseOnExceptions" => {
                let mode = match str_param("state") {
                    Some("none") => PauseOnExceptions::None,
                    Some("uncaught") => PauseOnExceptions::Uncaught,
                    Some("all") | Some("caught") => PauseOnExceptions::All,
                    _ => return Err(invalid("state")),
                };
                d.set_pause_on_exceptions(mode);
                Ok(json!({}))
            }
            "Debugger.setAsyncCallStackDepth" => {
                d.set_async_stack_depth(p.get("maxDepth").and_then(Json::as_u64).unwrap_or(0) as u32);
                Ok(json!({}))
            }
            "Debugger.pause" => {
                // A transport reading on its own thread has already raised
                // the request; raising it while paused would pause again.
                if !self.state.borrow().paused {
                    d.pause_requester().request();
                }
                Ok(json!({}))
            }
            "Debugger.resume" => self.resume(ResumeAction::Continue),
            "Debugger.stepOver" => self.resume(ResumeAction::StepOver),
            "Debugger.stepInto" => self.resume(ResumeAction::StepInto),
            "Debugger.stepOut" => self.resume(ResumeAction::StepOut),
            "Debugger.evaluateOnCallFrame" => {
                let frame = str_param("callFrameId")
                    .and_then(|s| s.parse::<usize>().ok())
                    .ok_or_else(|| invalid("callFrameId"))?;
                let expr = str_param("expression").ok_or_else(|| invalid("expression"))?;
                let group = str_param("objectGroup").map(str::to_string);
                let by_value = p.get("returnByValue").and_then(Json::as_bool).unwrap_or(false);
                self.state.borrow_mut().evaluating += 1;
                let r = d.evaluate_on_frame(frame, expr);
                self.state.borrow_mut().evaluating -= 1;
                match r {
                    Ok(v) => Ok(self.evaluation_result(d, Ok(v), group.as_deref(), by_value)),
                    Err(agnijs_js_debugger::DebugError::Engine(e)) => {
                        Ok(self.evaluation_result(d, Err(e), group.as_deref(), by_value))
                    }
                    Err(e) => Err((-32000, e.to_string())),
                }
            }
            // Accepted so clients' start-up sequences succeed; nothing to do.
            "Profiler.enable" | "Profiler.disable" | "HeapProfiler.enable"
            | "HeapProfiler.disable" | "Debugger.setBlackboxPatterns"
            | "Debugger.setBlackboxExecutionContexts" | "Runtime.setAsyncCallStackDepth" => {
                Ok(json!({}))
            }
            _ => Err((-32601, format!("'{method}' wasn't found"))),
        }
    }

    fn resume(&self, action: ResumeAction) -> Reply {
        let mut s = self.state.borrow_mut();
        if !s.paused {
            return Err((-32000, "Can only perform operation while paused.".into()));
        }
        s.resume = Some(action);
        Ok(json!({}))
    }

    fn release_group(&self, group: &str) {
        let mut s = self.state.borrow_mut();
        if let Some(ids) = s.groups.remove(group) {
            for id in ids {
                s.objects.remove(&id);
            }
        }
    }

    fn store(&self, remote: Remote, group: Option<&str>) -> String {
        let mut s = self.state.borrow_mut();
        s.next_object += 1;
        let id = s.next_object;
        s.objects.insert(id, remote);
        if let Some(g) = group {
            s.groups.entry(g.to_string()).or_default().push(id);
        }
        id.to_string()
    }

    /// A `Runtime.RemoteObject` for `v`.
    fn remote(&self, d: &mut dyn Debuggee, v: &Value, group: Option<&str>) -> Json {
        match v {
            Value::Undefined => json!({"type": "undefined"}),
            Value::Null => json!({"type": "object", "subtype": "null", "value": null}),
            Value::Bool(b) => json!({"type": "boolean", "value": b}),
            Value::Number(n) => {
                let mut o = json!({"type": "number", "description": d.describe(v).description});
                number_json(*n, &mut o);
                o
            }
            Value::String(s) => json!({"type": "string", "value": s.to_string_lossy()}),
            Value::BigInt(b) => json!({
                "type": "bigint",
                "unserializableValue": format!("{}n", b.0),
                "description": format!("{}n", b.0),
            }),
            Value::Symbol(_) => {
                let desc = d.describe(v).description;
                let id = self.store(Remote::Value(v.clone()), group);
                json!({"type": "symbol", "description": desc, "objectId": id})
            }
            Value::Object(_) => {
                let desc = d.describe(v);
                let id = self.store(Remote::Value(v.clone()), group);
                let mut o = json!({
                    "type": if desc.kind == ValueKind::Function { "function" } else { "object" },
                    "className": desc.class_name.unwrap_or_else(|| "Object".into()),
                    "description": desc.description,
                    "objectId": id,
                });
                if let Some(st) = desc.subtype {
                    o["subtype"] = json!(st.as_str());
                }
                o
            }
        }
    }

    /// `v` as JSON (`returnByValue`): objects through `JSON.stringify`.
    fn by_value(&self, d: &mut dyn Debuggee, v: &Value) -> Json {
        let mut o = match v {
            Value::Undefined => return json!({"type": "undefined"}),
            Value::Object(_) => {
                let desc = d.describe(v);
                let text = self.json_stringify(d, v).unwrap_or_else(|| "{}".into());
                let mut o = json!({
                    "type": if desc.kind == ValueKind::Function { "function" } else { "object" },
                    "value": serde_json::from_str::<Json>(&text).unwrap_or(json!({})),
                });
                if let Some(st) = desc.subtype {
                    o["subtype"] = json!(st.as_str());
                }
                return o;
            }
            _ => self.remote(d, v, None),
        };
        if let Some(m) = o.as_object_mut() {
            m.remove("objectId");
        }
        o
    }

    fn json_stringify(&self, d: &mut dyn Debuggee, v: &Value) -> Option<String> {
        let e = d.engine();
        let global = e.global_object(self.realm).ok()?;
        let json = e.get(self.realm, &global, &PropertyKey::from("JSON")).ok()?;
        let f = e.get(self.realm, &json, &PropertyKey::from("stringify")).ok()?;
        match e.call(self.realm, &f, &json, std::slice::from_ref(v)).ok()? {
            Value::String(s) => Some(s.to_string_lossy()),
            _ => None,
        }
    }

    fn evaluation_result(
        &self,
        d: &mut dyn Debuggee,
        r: Result<Value, Error>,
        group: Option<&str>,
        by_value: bool,
    ) -> Json {
        match r {
            Ok(v) if by_value => json!({ "result": self.by_value(d, &v) }),
            Ok(v) => json!({ "result": self.remote(d, &v, group) }),
            Err(Error::Exception(ex)) => {
                let exception = self.remote(d, &ex.value, group);
                let id = self.next_exception_id();
                let top = ex.stack.first();
                json!({
                    "result": exception.clone(),
                    "exceptionDetails": {
                        "exceptionId": id,
                        "text": "Uncaught",
                        "lineNumber": top.map(|f| f.line.saturating_sub(1)).unwrap_or(0),
                        "columnNumber": top.map(|f| f.column.saturating_sub(1)).unwrap_or(0),
                        "exception": exception,
                    },
                })
            }
            Err(e) => {
                let id = self.next_exception_id();
                json!({
                    "result": {"type": "undefined"},
                    "exceptionDetails": {
                        "exceptionId": id,
                        "text": e.to_string(),
                        "lineNumber": 0,
                        "columnNumber": 0,
                    },
                })
            }
        }
    }

    fn next_exception_id(&self) -> u32 {
        let mut s = self.state.borrow_mut();
        s.next_exception += 1;
        s.next_exception
    }

    fn lookup(&self, id: &str) -> Result<RemoteRef, (i64, String)> {
        let n: u64 = id.parse().map_err(|_| invalid("objectId"))?;
        let s = self.state.borrow();
        match s.objects.get(&n) {
            Some(Remote::Value(v)) => Ok(RemoteRef::Value(v.clone())),
            Some(Remote::Scope(vars)) => Ok(RemoteRef::Scope(vars.clone())),
            None => Err((-32000, "Could not find object with given id".into())),
        }
    }

    fn get_properties(&self, d: &mut dyn Debuggee, p: &Json) -> Reply {
        let id = p.get("objectId").and_then(Json::as_str).ok_or_else(|| invalid("objectId"))?;
        let accessors_only = p
            .get("accessorPropertiesOnly")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let group = self.group_of(id);
        match self.lookup(id)? {
            RemoteRef::Scope(vars) => {
                let result: Vec<Json> = vars
                    .iter()
                    .map(|(name, v)| {
                        json!({
                            "name": name,
                            "value": self.remote(d, v, group.as_deref()),
                            "writable": true,
                            "configurable": false,
                            "enumerable": true,
                            "isOwn": true,
                        })
                    })
                    .collect();
                Ok(json!({ "result": if accessors_only { vec![] } else { result } }))
            }
            RemoteRef::Value(v) => {
                let props = d.properties(&v).map_err(|e| (-32000, e.to_string()))?;
                let mut result = Vec::new();
                for prop in &props.properties {
                    let accessor = prop.get.is_some() || prop.set.is_some();
                    if accessors_only && !accessor {
                        continue;
                    }
                    let mut o = json!({
                        "configurable": prop.configurable,
                        "enumerable": prop.enumerable,
                        "isOwn": true,
                    });
                    match &prop.name {
                        PropertyName::String(s) => o["name"] = json!(s),
                        PropertyName::Symbol(sym) => {
                            o["name"] = json!(d.describe(sym).description);
                            o["symbol"] = self.remote(d, sym, group.as_deref());
                        }
                    }
                    if let Some(value) = &prop.value {
                        o["value"] = self.remote(d, value, group.as_deref());
                        o["writable"] = json!(prop.writable);
                    }
                    if let Some(g) = &prop.get {
                        o["get"] = self.remote(d, g, group.as_deref());
                    }
                    if let Some(s) = &prop.set {
                        o["set"] = self.remote(d, s, group.as_deref());
                    }
                    if accessor {
                        o.as_object_mut().unwrap().entry("get").or_insert(json!({"type": "undefined"}));
                        o.as_object_mut().unwrap().entry("set").or_insert(json!({"type": "undefined"}));
                    }
                    result.push(o);
                }
                let internal: Vec<Json> = if accessors_only {
                    Vec::new()
                } else {
                    props
                        .internal
                        .iter()
                        .map(|(name, v)| json!({ "name": name, "value": self.remote(d, v, group.as_deref()) }))
                        .collect()
                };
                Ok(json!({ "result": result, "internalProperties": internal }))
            }
        }
    }

    fn group_of(&self, id: &str) -> Option<String> {
        let n: u64 = id.parse().ok()?;
        let s = self.state.borrow();
        s.groups
            .iter()
            .find(|(_, ids)| ids.contains(&n))
            .map(|(g, _)| g.clone())
    }

    fn call_function_on(&self, d: &mut dyn Debuggee, p: &Json) -> Reply {
        let decl = p
            .get("functionDeclaration")
            .and_then(Json::as_str)
            .ok_or_else(|| invalid("functionDeclaration"))?;
        let this = match p.get("objectId").and_then(Json::as_str) {
            Some(id) => match self.lookup(id)? {
                RemoteRef::Value(v) => v,
                RemoteRef::Scope(_) => Value::Undefined,
            },
            None => Value::Undefined,
        };
        let mut args = Vec::new();
        for a in p.get("arguments").and_then(Json::as_array).into_iter().flatten() {
            args.push(self.call_argument(d, a)?);
        }
        let group = p.get("objectGroup").and_then(Json::as_str).map(str::to_string);
        let by_value = p.get("returnByValue").and_then(Json::as_bool).unwrap_or(false);
        self.state.borrow_mut().evaluating += 1;
        let r = d
            .engine()
            .eval_script(self.realm, EVALUATE_URL, &format!("({decl})"))
            .and_then(|f| d.engine().call(self.realm, &f, &this, &args));
        self.state.borrow_mut().evaluating -= 1;
        Ok(self.evaluation_result(d, r, group.as_deref(), by_value))
    }

    fn call_argument(&self, d: &mut dyn Debuggee, a: &Json) -> Result<Value, (i64, String)> {
        if let Some(id) = a.get("objectId").and_then(Json::as_str) {
            return match self.lookup(id)? {
                RemoteRef::Value(v) => Ok(v),
                RemoteRef::Scope(_) => Ok(Value::Undefined),
            };
        }
        if let Some(u) = a.get("unserializableValue").and_then(Json::as_str) {
            return d
                .engine()
                .eval_script(self.realm, EVALUATE_URL, u)
                .map_err(|e| (-32000, e.to_string()));
        }
        Ok(match a.get("value") {
            None => Value::Undefined,
            Some(Json::Null) => Value::Null,
            Some(Json::Bool(b)) => Value::Bool(*b),
            Some(Json::Number(n)) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
            Some(Json::String(s)) => Value::String(s.as_str().into()),
            Some(other) => d
                .engine()
                .eval_script(self.realm, EVALUATE_URL, &format!("({other})"))
                .map_err(|e| (-32000, e.to_string()))?,
        })
    }

    // ---- events ---------------------------------------------------------------------------

    fn script_parsed_event(&self, d: &mut dyn Debuggee, s: &ScriptInfo) {
        let source = d.script_source(s.id).unwrap_or_default();
        let hash: String = ws::sha1(source.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut params = json!({
            "scriptId": s.id.0.to_string(),
            "url": s.url,
            "startLine": 0,
            "startColumn": 0,
            "endLine": s.end_line,
            "endColumn": s.end_column,
            "executionContextId": s.realm.0 + 1,
            "hash": hash,
            "isModule": s.kind == agnijs_js_debugger::ScriptKind::Module,
            "length": source.encode_utf16().count(),
            "scriptLanguage": "JavaScript",
        });
        if let Some(m) = &s.source_map {
            // A map the host handed over with the source: give the client
            // the text itself, as an inline data URL.
            let url = format!(
                "data:application/json;base64,{}",
                ws::base64_encode(m.as_bytes())
            );
            params["sourceMapURL"] = json!(url);
        } else if let Some(m) = &s.source_map_url {
            params["sourceMapURL"] = json!(m);
        }
        self.event("Debugger.scriptParsed", params);
    }

    fn frame_json(&self, d: &mut dyn Debuggee, f: &FrameInfo) -> Json {
        let group = Some("backtrace");
        let scopes: Vec<Json> = f
            .scopes
            .iter()
            .map(|s| {
                let object = match &s.contents {
                    ScopeContents::Object(v) => self.remote(d, v, group),
                    ScopeContents::Variables(vars) => {
                        let id = self.store(Remote::Scope(vars.clone()), group);
                        json!({
                            "type": "object",
                            "className": "Object",
                            "description": "Object",
                            "objectId": id,
                        })
                    }
                };
                json!({ "type": scope_type(s.kind), "object": object })
            })
            .collect();
        json!({
            "callFrameId": f.index.to_string(),
            "functionName": f.function_name,
            "location": location_json(&f.location),
            "url": f.url,
            "scopeChain": scopes,
            "this": self.remote(d, &f.this, group),
        })
    }

    /// The source map of a script, loaded once: the host-supplied map when
    /// there is one, else the one its `sourceMappingURL` comment names.
    fn source_map(&self, d: &mut dyn Debuggee, script: ScriptId) -> Option<Rc<SourceMap>> {
        if let Some(m) = self.state.borrow().source_maps.get(&script) {
            return m.clone();
        }
        let info = d.scripts().into_iter().find(|s| s.id == script);
        let map = info.and_then(|info| {
            if let Some(text) = &info.source_map {
                return SourceMap::parse(text).ok();
            }
            let url = info.source_map_url.as_deref()?;
            match SourceMap::from_data_url(url) {
                Some(r) => r.ok(),
                None => {
                    let text = self.loader.borrow().as_ref()?(&info.url, url)?;
                    SourceMap::parse(&text).ok()
                }
            }
        });
        let map = map.map(Rc::new);
        self.state
            .borrow_mut()
            .source_maps
            .insert(script, map.clone());
        map
    }

    /// `    at name (source:line:column)` with the position mapped to the
    /// original source when the script has a source map.
    fn mapped_frame_line(&self, d: &mut dyn Debuggee, f: &StackFrameInfo) -> String {
        let name = if f.function_name.is_empty() {
            "<anonymous>"
        } else {
            &f.function_name
        };
        let mapped = f
            .script
            .and_then(|s| self.source_map(d, s))
            .and_then(|m| m.lookup(f.line, f.column));
        match mapped {
            Some(o) => format!("    at {name} ({}:{}:{})", o.source, o.line + 1, o.column + 1),
            None => format!("    at {name} ({}:{}:{})", f.url, f.line + 1, f.column + 1),
        }
    }
}

enum RemoteRef {
    Value(Value),
    Scope(Vec<(String, Value)>),
}

impl DebugClient for Session {
    fn script_parsed(&self, d: &mut dyn Debuggee, script: &ScriptInfo) {
        if self.state.borrow().debugger_enabled {
            self.script_parsed_event(d, script);
        }
    }

    fn paused(&self, d: &mut dyn Debuggee, pause: &PauseInfo) -> ResumeAction {
        if !self.state.borrow().debugger_enabled {
            return ResumeAction::Continue;
        }
        let frames: Vec<Json> = pause.frames.iter().map(|f| self.frame_json(d, f)).collect();
        let mut params = json!({ "callFrames": frames, "reason": "other" });
        match &pause.reason {
            PauseReason::Breakpoint(ids) => {
                params["hitBreakpoints"] = json!(ids.iter().map(|b| b.0.to_string()).collect::<Vec<_>>());
            }
            PauseReason::Exception { value, uncaught } => {
                params["reason"] = json!("exception");
                params["data"] = self.remote(d, value, Some("backtrace"));
                params["data"]["uncaught"] = json!(uncaught);
            }
            PauseReason::Requested => params["reason"] = json!("debugCommand"),
            PauseReason::Step | PauseReason::DebuggerStatement => {}
        }
        if let Some(a) = &pause.async_stack {
            params["asyncStackTrace"] = async_json(a);
        }
        {
            let mut s = self.state.borrow_mut();
            s.paused = true;
            s.resume = None;
        }
        self.event("Debugger.paused", params);
        let action = loop {
            if let Some(a) = self.state.borrow_mut().resume.take() {
                break a;
            }
            match self.transport.recv(true) {
                Incoming::Message(m) => self.handle(d, &m),
                Incoming::Empty => {}
                Incoming::Disconnected => self.disconnected(d),
                Incoming::Closed => break ResumeAction::Continue,
            }
        };
        self.state.borrow_mut().paused = false;
        self.release_group("backtrace");
        self.event("Debugger.resumed", json!({}));
        action
    }

    fn console(&self, d: &mut dyn Debuggee, realm: RealmId, level: ConsoleLevel, args: &[Value]) {
        if !self.state.borrow().runtime_enabled {
            return;
        }
        let args: Vec<Json> = args.iter().map(|v| self.remote(d, v, Some("console"))).collect();
        self.event(
            "Runtime.consoleAPICalled",
            json!({
                "type": level_name(level),
                "args": args,
                "executionContextId": realm.0 + 1,
                "timestamp": timestamp(),
            }),
        );
    }

    fn exception_thrown(&self, d: &mut dyn Debuggee, e: &ExceptionEvent) {
        {
            let s = self.state.borrow();
            if !s.runtime_enabled || s.evaluating > 0 {
                return;
            }
        }
        let mut exception = self.remote(d, &e.value, Some("console"));
        // The description carries the stack, mapped to original sources.
        let headline = e.text.strip_prefix("Uncaught ").unwrap_or(&e.text).to_string();
        let mut description = headline;
        for f in &e.stack {
            description.push('\n');
            description.push_str(&self.mapped_frame_line(d, f));
        }
        if exception.get("objectId").is_some() {
            exception["description"] = json!(description);
        }
        let top = e.stack.first();
        let id = self.next_exception_id();
        let mut details = json!({
            "exceptionId": id,
            "text": "Uncaught",
            "lineNumber": top.map(|f| f.line).unwrap_or(0),
            "columnNumber": top.map(|f| f.column).unwrap_or(0),
            "exception": exception,
            "executionContextId": self.realm.0 + 1,
            "stackTrace": { "callFrames": e.stack.iter().map(stack_frame_json).collect::<Vec<_>>() },
        });
        if let Some(f) = top {
            details["url"] = json!(f.url);
            if let Some(s) = f.script {
                details["scriptId"] = json!(s.0.to_string());
            }
        }
        if e.stack.is_empty() {
            // A syntax error: the message is all there is.
            details["text"] = json!(e.text);
        }
        self.event(
            "Runtime.exceptionThrown",
            json!({ "timestamp": timestamp(), "exceptionDetails": details }),
        );
    }
}
