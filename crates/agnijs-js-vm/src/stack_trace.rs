//! Stack traces (TASK-103.17): `Error.prototype.stack`, `Error.captureStackTrace`,
//! `Error.stackTraceLimit`, `Error.prepareStackTrace` and call sites, the
//! frames reported to the host in `Exception::stack`, and host-supplied
//! source maps applied to both.
//!
//! The cost model: nothing happens until an error object is created. Then
//! the frames are captured as code-block references and instruction
//! indices (no positions are computed, no strings are built); the text is
//! formatted at the first read of `stack` and cached, and the captured
//! frames are dropped. Async frames are not tracked as code runs: at
//! capture, when the bottom frame is an async function resumed from the
//! job queue, the chain of async functions awaiting it is read off the
//! promise reactions (an `await` leaves an `Await` handler on the awaited
//! promise naming the awaiting function's state object).
//!
//! The text follows V8's format so common tooling can parse it:
//!
//! ```text
//! TypeError: boom
//!     at inner (app.js:2:9)
//!     at Foo.method (app.js:7:5)
//!     at new Foo (app.js:11:3)
//!     at async outer (app.js:15:3)
//!     at app.js:20:1
//! ```
//!
//! Lines and columns are 1-based, columns in UTF-16 code units. A source
//! map registered for a script name (`Engine::set_source_map`, or a module
//! source loaded with its map) rewrites the file, line and column of every
//! frame in that script to the original position.

use crate::builtins::promise::{Handler, Reactions, ThenRecord};
use crate::gc::Tracer;
use crate::interp::generator::GenKind;
use crate::interp::{ConstructKind, Frame, FrameKind};
use crate::object::{CallArgs, Class, ObjectData};
use crate::realm::Intrinsic;
use crate::value::{Attrs, ObjId, PropKey, Property, PropertyDescriptor, Value};
use crate::vm::{JsResult, Vm};
use agnijs_engine_api::StackFrame;
use agnijs_js_compiler::bytecode::{CodeBlock, CodeKind, SourceText};
use agnijs_js_debugger::SourceMap;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The largest `Error.stackTraceLimit` honoured.
const MAX_FRAMES: usize = 1000;
/// Frames recorded for the host's `Exception::stack` while a throw unwinds.
pub const MAX_UNWOUND_FRAMES: usize = 64;
/// Scripts whose line tables are kept between formats.
const LINE_CACHE: usize = 8;

/// The frame was a `new` call.
pub const FRAME_CONSTRUCT: u8 = 1;
/// The frame is an async function suspended at an `await`, found through
/// the promise it awaits.
pub const FRAME_ASYNC: u8 = 2;

/// One frame as captured at error creation.
pub struct CapturedFrame {
    pub code: Rc<CodeBlock>,
    /// The instruction the frame was at (index of the call or await).
    pub pc: u32,
    pub this: Value,
    pub callee: Option<ObjId>,
    pub flags: u8,
    pub kind: FrameKind,
}

impl CapturedFrame {
    /// A function frame, as opposed to script, module or eval code.
    pub fn is_function(&self) -> bool {
        self.callee.is_some() && is_function_code(&self.code)
    }

    fn of(f: &Frame, flags: u8) -> CapturedFrame {
        let construct = if matches!(f.construct, ConstructKind::None) {
            0
        } else {
            FRAME_CONSTRUCT
        };
        CapturedFrame {
            code: f.code.clone(),
            pc: f.pc.saturating_sub(1) as u32,
            this: f.this,
            callee: f.callee,
            flags: flags | construct,
            kind: f.kind,
        }
    }
}

/// The payload of an error object's [[ErrorData]] slot: its captured
/// frames until `stack` is first read, the formatted value after.
pub struct ErrorStack {
    pub frames: Vec<CapturedFrame>,
    pub formatted: Option<Value>,
}

impl ErrorStack {
    pub fn trace(&self, t: &mut Tracer) {
        for f in &self.frames {
            t.value(f.this);
            if let Some(c) = f.callee {
                t.obj(c);
            }
        }
        if let Some(v) = self.formatted {
            t.value(v);
        }
    }
}

/// A frame unwound by a throw on its way to the host, resolved to a
/// position only if the throw escapes.
pub struct UnwoundFrame {
    pub code: Rc<CodeBlock>,
    pub pc: u32,
    pub named: bool,
}

struct MapEntry {
    text: Rc<str>,
    /// `None` until first used; `Some(None)` when the map does not decode.
    parsed: Option<Option<Rc<SourceMap>>>,
}

/// Per-VM stack-trace state.
#[derive(Default)]
pub struct StackState {
    source_maps: HashMap<Rc<str>, MapEntry>,
    lines: Vec<(Weak<SourceText>, Rc<[u32]>)>,
    /// Nesting of `Error.prepareStackTrace` calls: inside one, traces use
    /// the default format.
    preparing: u32,
}

/// The data of a CallSite object handed to `Error.prepareStackTrace`.
pub struct CallSiteData {
    pub this: Value,
    pub function: Value,
    pub file: Value,
    pub line: Value,
    pub column: Value,
    pub function_name: Value,
    pub type_name: Value,
    pub method_name: Value,
    pub text: Value,
    pub flags: u8,
}

const SITE_TOPLEVEL: u8 = 4;
const SITE_EVAL: u8 = 8;

impl CallSiteData {
    pub fn trace(&self, t: &mut Tracer) {
        for v in [
            self.this,
            self.function,
            self.file,
            self.line,
            self.column,
            self.function_name,
            self.type_name,
            self.method_name,
            self.text,
        ] {
            t.value(v);
        }
    }
}

/// A resolved position: file, 1-based line, 1-based UTF-16 column.
struct Position {
    file: String,
    line: u32,
    column: u32,
}

impl Vm {
    // ---- source maps ----------------------------------------------------------------

    /// Register (or with `None` remove) the source map for the script or
    /// module named `script`. The map is decoded at first use; one that
    /// does not decode is ignored.
    pub fn set_source_map(&mut self, script: &str, map: Option<&str>) {
        match map {
            Some(text) => {
                self.stacks.source_maps.insert(
                    Rc::from(script),
                    MapEntry {
                        text: Rc::from(text),
                        parsed: None,
                    },
                );
            }
            None => {
                self.stacks.source_maps.remove(script);
            }
        }
    }

    /// The JSON text of the source map registered for `script`.
    pub fn source_map_text(&self, script: &str) -> Option<Rc<str>> {
        self.stacks.source_maps.get(script).map(|e| e.text.clone())
    }

    fn source_map(&mut self, script: &str) -> Option<Rc<SourceMap>> {
        let e = self.stacks.source_maps.get_mut(script)?;
        if e.parsed.is_none() {
            e.parsed = Some(SourceMap::parse(&e.text).ok().map(Rc::new));
        }
        e.parsed.clone().flatten()
    }

    // ---- positions ------------------------------------------------------------------

    fn line_starts(&mut self, source: &Rc<SourceText>) -> Rc<[u32]> {
        let w = Rc::downgrade(source);
        if let Some(i) = self.stacks.lines.iter().position(|(s, _)| Weak::ptr_eq(s, &w)) {
            let hit = self.stacks.lines.remove(i);
            let t = hit.1.clone();
            self.stacks.lines.push(hit);
            return t;
        }
        let mut starts = vec![0u32];
        for (i, b) in source.text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        let t: Rc<[u32]> = starts.into();
        self.stacks.lines.retain(|(s, _)| s.strong_count() > 0);
        if self.stacks.lines.len() >= LINE_CACHE {
            self.stacks.lines.remove(0);
        }
        self.stacks.lines.push((w, t.clone()));
        t
    }

    /// The generated position of instruction `pc` of `code`.
    fn generated_position(&mut self, code: &CodeBlock, pc: u32) -> Position {
        let source = code.source.clone();
        let text: &str = &source.text;
        let off = (code.offset_at(pc as usize) as usize).min(text.len());
        let starts = self.line_starts(&source);
        let line = starts.partition_point(|&s| s as usize <= off).max(1) - 1;
        let start = starts[line] as usize;
        let column = text
            .get(start..off)
            .map(|s| {
                if s.is_ascii() {
                    s.len()
                } else {
                    s.encode_utf16().count()
                }
            })
            .unwrap_or(0);
        Position {
            file: source.name.to_string(),
            line: line as u32 + 1,
            column: column as u32 + 1,
        }
    }

    /// The position of instruction `pc` of `code`, through the script's
    /// source map when the host registered one.
    fn mapped_position(&mut self, code: &CodeBlock, pc: u32) -> Position {
        let p = self.generated_position(code, pc);
        if self.stacks.source_maps.is_empty() {
            return p;
        }
        let Some(map) = self.source_map(&code.source.name) else {
            return p;
        };
        match map.lookup(p.line - 1, p.column - 1) {
            Some(o) => Position {
                file: o.source,
                line: o.line.saturating_add(1),
                column: o.column.saturating_add(1),
            },
            None => p,
        }
    }

    // ---- capture --------------------------------------------------------------------

    /// `Error.stackTraceLimit` of the current realm: `None` (capture
    /// nothing) when it is not a number.
    fn stack_trace_limit(&self) -> Option<usize> {
        let ctor = self.intrinsic(Intrinsic::Error);
        let key = PropKey::Str(self.names.stackTraceLimit);
        match self.heap.get_own_property(ctor, key)?.value()? {
            Value::Number(n) if n.is_nan() || n <= 0.0 => Some(0),
            Value::Number(n) => Some(if n >= MAX_FRAMES as f64 {
                MAX_FRAMES
            } else {
                n as usize
            }),
            _ => None,
        }
    }

    /// The current frames, innermost first, up to `limit`. With `skip_to`,
    /// frames up to and including the innermost call of that function are
    /// left out (none are kept when it is not on the stack).
    fn capture_frames(&mut self, limit: usize, skip_to: Option<ObjId>) -> Vec<CapturedFrame> {
        let mut out = Vec::new();
        if limit == 0 {
            return out;
        }
        let mut skipping = false;
        if let Some(f) = skip_to {
            if !self.frames.iter().any(|fr| fr.callee == Some(f)) {
                return out;
            }
            skipping = true;
        }
        for fr in self.frames.iter().rev() {
            if skipping {
                if fr.callee == skip_to {
                    skipping = false;
                }
                continue;
            }
            if out.len() >= limit {
                return out;
            }
            out.push(CapturedFrame::of(fr, 0));
        }
        // The bottom frame is an async body resumed by a job: continue
        // with the async functions awaiting it.
        let mut gen = self.frames.first().and_then(|f| f.generator);
        while let Some(g) = gen {
            if out.len() >= limit {
                break;
            }
            let Some(p) = self.async_result_promise(g) else { break };
            let Some(next) = self.awaiting_generator(p) else { break };
            let Class::Generator(d) = &self.heap.obj(next).class else { break };
            let Some(saved) = &d.saved else { break };
            out.push(CapturedFrame::of(&saved.frame, FRAME_ASYNC));
            gen = Some(next);
        }
        out
    }

    /// The promise an async function or async generator body settles.
    fn async_result_promise(&self, gen: ObjId) -> Option<ObjId> {
        let Class::Generator(d) = &self.heap.obj(gen).class else {
            return None;
        };
        match d.kind {
            GenKind::AsyncFn => d.promise.as_ref().map(|c| c.promise),
            GenKind::Async => d.queue.front().map(|r| r.capability.promise),
            GenKind::Sync => None,
        }
    }

    /// The async body suspended in an `await` of promise `p`.
    fn awaiting_generator(&self, p: ObjId) -> Option<ObjId> {
        let Class::Promise(d) = &self.heap.obj(p).class else {
            return None;
        };
        fn awaiting(r: &ThenRecord) -> Option<ObjId> {
            match (&r.on_fulfilled, &r.on_rejected) {
                (Handler::Await { gen, .. }, _) | (_, Handler::Await { gen, .. }) => Some(*gen),
                _ => None,
            }
        }
        match &d.reactions {
            Reactions::None => None,
            Reactions::One(r) => awaiting(r),
            Reactions::Many(rs) => rs.iter().find_map(awaiting),
        }
    }

    /// Capture the current stack into a fresh error object `o` (called by
    /// every path that creates an object with [[ErrorData]]).
    pub fn attach_error_stack(&mut self, o: ObjId) {
        let Some(limit) = self.stack_trace_limit() else {
            return;
        };
        let frames = self.capture_frames(limit, None);
        if let Class::Error(slot) = &mut self.heap.obj_mut(o).class {
            *slot = Some(Box::new(ErrorStack {
                frames,
                formatted: None,
            }));
        }
    }

    // ---- formatting -----------------------------------------------------------------

    /// The header line of an error's trace, `name: message` as
    /// Error.prototype.toString builds it, from data properties only.
    fn stack_header(&mut self, o: ObjId) -> String {
        let name_key = PropKey::Str(self.names.name);
        let name = match self.lookup_data_property(o, name_key) {
            Some(Value::String(s)) => self.heap.str_to_string(s),
            None | Some(Value::Undefined) => "Error".to_string(),
            Some(v) => self.display(v),
        };
        let msg_key = PropKey::Str(self.names.message);
        let msg = match self.lookup_data_property(o, msg_key) {
            Some(Value::String(s)) => self.heap.str_to_string(s),
            None | Some(Value::Undefined) => String::new(),
            Some(v) => self.display(v),
        };
        match (name.is_empty(), msg.is_empty()) {
            (true, _) => msg,
            (_, true) => name,
            _ => format!("{name}: {msg}"),
        }
    }

    fn is_global_object(&self, v: Value) -> bool {
        matches!(v, Value::Object(o) if self.realms.iter().any(|r| r.global == o))
    }

    /// The type name V8 prints before a method's name: the receiver's
    /// constructor's name (or, for a function receiver, its own name).
    fn receiver_type_name(&mut self, this: Value) -> Option<String> {
        let Value::Object(o) = this else { return None };
        let name_key = PropKey::Str(self.names.name);
        let named = |vm: &mut Vm, f: ObjId| match vm.lookup_data_property(f, name_key) {
            Some(Value::String(s)) => Some(vm.heap.str_to_string(s)).filter(|s| !s.is_empty()),
            _ => None,
        };
        if self.heap.obj(o).is_callable() {
            return named(self, o).or_else(|| Some("Function".into()));
        }
        let ctor_key = PropKey::Str(self.names.constructor);
        match self.lookup_data_property(o, ctor_key) {
            Some(Value::Object(c)) => named(self, c),
            _ => None,
        }
    }

    /// Whether the frame is a method call: a function frame with a
    /// receiver other than undefined, null or a global object.
    fn is_method_call(&self, f: &CapturedFrame) -> bool {
        f.is_function()
            && f.flags & FRAME_CONSTRUCT == 0
            && !matches!(f.code.kind, CodeKind::Arrow)
            && !matches!(f.this, Value::Undefined | Value::Null | Value::Empty)
            && !self.is_global_object(f.this)
    }

    /// The function's name: the compiled one, else (a method or a
    /// function named at run time) the callee's own `name` data property.
    fn frame_function_name(&mut self, f: &CapturedFrame) -> String {
        if !f.code.name.is_empty() {
            return f.code.name.to_string();
        }
        let Some(c) = f.callee else { return String::new() };
        let key = PropKey::Str(self.names.name);
        match self.heap.get_own_property(c, key).and_then(|p| p.value()) {
            Some(Value::String(s)) => self.heap.str_to_string(s),
            _ => String::new(),
        }
    }

    /// The function part of a frame line (`new Foo`, `async f`,
    /// `Foo.bar`), empty for top-level code.
    fn frame_function_text(&mut self, f: &CapturedFrame) -> String {
        if matches!(f.kind, FrameKind::Eval) {
            return "eval".into();
        }
        if !f.is_function() {
            return String::new();
        }
        let name = self.frame_function_name(f);
        let name: &str = &name;
        let mut s = String::new();
        if f.flags & FRAME_ASYNC != 0 {
            s.push_str("async ");
        }
        if f.flags & FRAME_CONSTRUCT != 0 {
            s.push_str("new ");
            s.push_str(if name.is_empty() { "<anonymous>" } else { name });
            return s;
        }
        if self.is_method_call(f) {
            let ty = self.receiver_type_name(f.this);
            match (ty, name.is_empty()) {
                (Some(t), true) => {
                    s.push_str(&t);
                    s.push_str(".<anonymous>");
                }
                (Some(t), false) if !name.starts_with(&format!("{t}.")) => {
                    s.push_str(&t);
                    s.push('.');
                    s.push_str(name);
                }
                (_, false) => s.push_str(name),
                (None, true) => s.push_str("<anonymous>"),
            }
            return s;
        }
        if name.is_empty() {
            if f.flags & FRAME_ASYNC != 0 {
                s.push_str("<anonymous>");
            }
            return s;
        }
        s.push_str(name);
        s
    }

    /// One `    at ...` line.
    fn frame_line(&mut self, f: &CapturedFrame, out: &mut String) {
        let func = self.frame_function_text(f);
        let p = self.mapped_position(&f.code, f.pc);
        out.push_str("\n    at ");
        if func.is_empty() {
            out.push_str(&format!("{}:{}:{}", p.file, p.line, p.column));
        } else {
            out.push_str(&format!("{func} ({}:{}:{})", p.file, p.line, p.column));
        }
    }

    fn default_stack_text(&mut self, o: ObjId, frames: &[CapturedFrame]) -> String {
        let mut s = self.stack_header(o);
        for f in frames {
            self.frame_line(f, &mut s);
        }
        s
    }

    /// The value of `o.stack` for an error with a captured trace: the cached
    /// value, or the trace formatted now (by `Error.prepareStackTrace` when
    /// it is a function) and cached.
    pub fn error_stack_value(&mut self, o: ObjId) -> JsResult<Value> {
        let frames: Vec<CapturedFrame> = match &self.heap.obj(o).class {
            Class::Error(Some(s)) => {
                if let Some(v) = s.formatted {
                    return Ok(v);
                }
                s.frames
                    .iter()
                    .map(|f| CapturedFrame {
                        code: f.code.clone(),
                        pc: f.pc,
                        this: f.this,
                        callee: f.callee,
                        flags: f.flags,
                        kind: f.kind,
                    })
                    .collect()
            }
            _ => return Ok(Value::Undefined),
        };
        let ctor = self.intrinsic(Intrinsic::Error);
        let key = PropKey::Str(self.names.prepareStackTrace);
        let prepare = self
            .heap
            .get_own_property(ctor, key)
            .and_then(|p| p.value())
            .filter(|v| self.is_callable(*v));
        let v = match prepare {
            Some(f) if self.stacks.preparing == 0 => {
                let mark = self.root_mark();
                self.root(Value::Object(o));
                let sites = self.call_sites(&frames);
                for s in &sites {
                    self.root(Value::Object(*s));
                }
                let arr = self.new_array(sites.into_iter().map(Value::Object).collect());
                self.root(Value::Object(arr));
                self.stacks.preparing += 1;
                let r = self.call_value(f, Value::Object(ctor), &[Value::Object(o), Value::Object(arr)]);
                self.stacks.preparing -= 1;
                self.pop_roots(mark);
                // A throwing prepareStackTrace leaves the trace unformatted.
                r?
            }
            _ => {
                let text = self.default_stack_text(o, &frames);
                self.str(&text)
            }
        };
        if let Class::Error(Some(s)) = &mut self.heap.obj_mut(o).class {
            s.frames = Vec::new();
            s.formatted = Some(v);
        }
        Ok(v)
    }

    /// The `stack` text of `o` without running any JavaScript, for the
    /// debugger: the cached value when it is a string, else the default
    /// format of the captured frames.
    pub fn error_stack_text(&mut self, o: ObjId) -> Option<String> {
        if let Some(Value::String(s)) = self
            .heap
            .get_own_property(o, PropKey::Str(self.names.stack))
            .and_then(|p| p.value())
        {
            return Some(self.heap.str_to_string(s));
        }
        let Class::Error(Some(s)) = &self.heap.obj(o).class else {
            return None;
        };
        if let Some(v) = s.formatted {
            return match v {
                Value::String(s) => Some(self.heap.str_to_string(s)),
                _ => None,
            };
        }
        let frames: Vec<CapturedFrame> = s
            .frames
            .iter()
            .map(|f| CapturedFrame {
                code: f.code.clone(),
                pc: f.pc,
                this: f.this,
                callee: f.callee,
                flags: f.flags,
                kind: f.kind,
            })
            .collect();
        Some(self.default_stack_text(o, &frames))
    }

    /// CallSite objects for `frames`.
    fn call_sites(&mut self, frames: &[CapturedFrame]) -> Vec<ObjId> {
        let proto = self.call_site_prototype();
        let mark = self.root_mark();
        self.root(Value::Object(proto));
        let mut out = Vec::with_capacity(frames.len());
        for f in frames {
            let g = self.generated_position(&f.code, f.pc);
            let mut line = String::new();
            self.frame_line(f, &mut line);
            let text = self.str(line.trim_start_matches("\n    at "));
            self.root(text);
            let strict = f.code.strict;
            let method = self.is_method_call(f);
            let type_name = match self.receiver_type_name(f.this) {
                Some(t) if method => self.str(&t),
                _ => Value::Null,
            };
            self.root(type_name);
            let name = self.frame_function_name(f);
            let function_name = if !f.is_function() || name.is_empty() {
                Value::Null
            } else {
                self.str(&name)
            };
            self.root(function_name);
            let method_name = if method && !name.is_empty() {
                function_name
            } else {
                Value::Null
            };
            let file = self.str(&g.file);
            self.root(file);
            let mut flags = f.flags;
            if !f.is_function() || (!method && f.flags & FRAME_CONSTRUCT == 0) {
                flags |= SITE_TOPLEVEL;
            }
            if matches!(f.kind, FrameKind::Eval) {
                flags |= SITE_EVAL;
            }
            let data = CallSiteData {
                this: if strict { Value::Undefined } else { f.this },
                function: match (strict, f.callee) {
                    (false, Some(c)) => Value::Object(c),
                    _ => Value::Undefined,
                },
                file,
                line: Value::Number(g.line as f64),
                column: Value::Number(g.column as f64),
                function_name,
                type_name,
                method_name,
                text,
                flags,
            };
            let s = self.alloc(ObjectData::new(Some(proto), Class::CallSite(Box::new(data))));
            self.root(Value::Object(s));
            out.push(s);
        }
        self.pop_roots(mark);
        out
    }

    /// The prototype of CallSite objects (made per `prepareStackTrace`
    /// call, so it is not a realm intrinsic and never in a snapshot).
    fn call_site_prototype(&mut self) -> ObjId {
        let object_proto = self.intrinsic(Intrinsic::ObjectPrototype);
        let p = self.new_object_with_proto(Some(object_proto));
        let mark = self.root_mark();
        self.root(Value::Object(p));
        let methods: [(&str, crate::object::NativeFn); 18] = [
            ("getThis", site_get_this),
            ("getTypeName", site_get_type_name),
            ("getFunction", site_get_function),
            ("getFunctionName", site_get_function_name),
            ("getMethodName", site_get_method_name),
            ("getFileName", site_get_file_name),
            ("getLineNumber", site_get_line_number),
            ("getColumnNumber", site_get_column_number),
            ("getEvalOrigin", site_get_eval_origin),
            ("getScriptNameOrSourceURL", site_get_file_name),
            ("isToplevel", site_is_toplevel),
            ("isEval", site_is_eval),
            ("isNative", site_false),
            ("isConstructor", site_is_constructor),
            ("isAsync", site_is_async),
            ("isPromiseAll", site_false),
            ("getPromiseIndex", site_null),
            ("toString", site_to_string),
        ];
        for (name, f) in methods {
            self.method(p, name, 0, f);
        }
        self.pop_roots(mark);
        p
    }

    // ---- unwinding to the host -----------------------------------------------------

    /// The host's view of the frames a throw unwound.
    pub(crate) fn resolve_unwound(&mut self, frames: Vec<UnwoundFrame>) -> Vec<StackFrame> {
        frames
            .into_iter()
            .map(|f| {
                let p = self.mapped_position(&f.code, f.pc);
                StackFrame {
                    function: if f.named { f.code.name.to_string() } else { String::new() },
                    script: p.file,
                    line: p.line,
                    column: p.column,
                }
            })
            .collect()
    }
}

/// Whether `code` is a function body (a module body runs as an async
/// function, but is reported as top-level code).
pub fn is_function_code(code: &CodeBlock) -> bool {
    !matches!(code.kind, CodeKind::Script | CodeKind::Module | CodeKind::Eval)
}

// ---- Error.prototype.stack, Error.captureStackTrace ------------------------------------

/// get Error.prototype.stack
pub fn stack_getter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("Error.prototype.stack getter called on non-object");
    };
    vm.error_stack_value(o)
}

/// set Error.prototype.stack: SetterThatIgnoresPrototypeProperties.
pub fn stack_setter(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.this else {
        return vm.type_error("Error.prototype.stack setter called on non-object");
    };
    let v = a.arg(0);
    if !matches!(v, Value::String(_)) {
        return vm.type_error("Error.prototype.stack must be set to a string");
    }
    let home = vm.intrinsic(Intrinsic::ErrorPrototype);
    if o == home {
        return vm.type_error("Cannot set stack on Error.prototype");
    }
    let key = PropKey::Str(vm.names.stack);
    if vm.own_property(o, key)?.is_none() {
        vm.create_data_property_or_throw(o, key, v)?;
    } else {
        vm.set(o, key, v, true)?;
    }
    Ok(Value::Undefined)
}

/// Error.captureStackTrace(targetObject[, constructorOpt])
pub fn capture_stack_trace(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    let Value::Object(o) = a.arg(0) else {
        return vm.type_error("Error.captureStackTrace: target is not an object");
    };
    let skip_to = match a.arg(1) {
        Value::Object(f) if vm.heap.obj(f).is_callable() => Some(f),
        _ => None,
    };
    let Some(limit) = vm.stack_trace_limit() else {
        return Ok(Value::Undefined);
    };
    // The innermost frame is the caller of captureStackTrace (natives
    // have no frame of their own).
    let frames = vm.capture_frames(limit, skip_to);
    let key = PropKey::Str(vm.names.stack);
    let own = vm.own_property(o, key)?;
    if own.is_none() {
        if let Class::Error(slot) = &mut vm.heap.obj_mut(o).class {
            *slot = Some(Box::new(ErrorStack {
                frames,
                formatted: None,
            }));
            return Ok(Value::Undefined);
        }
    }
    let mark = vm.root_mark();
    for f in &frames {
        vm.root(f.this);
    }
    let text = vm.default_stack_text(o, &frames);
    vm.pop_roots(mark);
    let text = vm.str(&text);
    vm.define_property_or_throw(o, key, PropertyDescriptor::data(text, Attrs::METHOD))?;
    Ok(Value::Undefined)
}

/// Install `Error.prototype.stack`, `Error.stackTraceLimit` and
/// `Error.captureStackTrace`.
pub fn install(vm: &mut Vm, error_ctor: ObjId, error_proto: ObjId) {
    vm.accessor(error_proto, "stack", stack_getter, Some(stack_setter));
    let key = PropKey::Str(vm.names.stackTraceLimit);
    vm.heap
        .add_own_property(error_ctor, key, Property::data(Value::Number(10.0), Attrs::ALL));
    vm.method(error_ctor, "captureStackTrace", 2, capture_stack_trace);
}

// ---- CallSite methods ---------------------------------------------------------------------

fn site<'v>(vm: &'v mut Vm, a: &CallArgs) -> JsResult<&'v CallSiteData> {
    if let Value::Object(o) = a.this {
        if matches!(vm.heap.obj(o).class, Class::CallSite(_)) {
            let Class::CallSite(d) = &vm.heap.obj(o).class else {
                unreachable!()
            };
            return Ok(d);
        }
    }
    vm.type_error("CallSite method called on an incompatible receiver")
}

macro_rules! site_field {
    ($name:ident, $field:ident) => {
        fn $name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
            Ok(site(vm, a)?.$field)
        }
    };
}

site_field!(site_get_this, this);
site_field!(site_get_function, function);
site_field!(site_get_file_name, file);
site_field!(site_get_line_number, line);
site_field!(site_get_column_number, column);
site_field!(site_get_function_name, function_name);
site_field!(site_get_type_name, type_name);
site_field!(site_get_method_name, method_name);
site_field!(site_to_string, text);

macro_rules! site_flag {
    ($name:ident, $flag:expr) => {
        fn $name(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
            Ok(Value::Bool(site(vm, a)?.flags & $flag != 0))
        }
    };
}

site_flag!(site_is_toplevel, SITE_TOPLEVEL);
site_flag!(site_is_eval, SITE_EVAL);
site_flag!(site_is_constructor, FRAME_CONSTRUCT);
site_flag!(site_is_async, FRAME_ASYNC);

fn site_get_eval_origin(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    site(vm, a)?;
    Ok(Value::Undefined)
}

fn site_false(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    site(vm, a)?;
    Ok(Value::Bool(false))
}

fn site_null(vm: &mut Vm, a: &CallArgs) -> JsResult<Value> {
    site(vm, a)?;
    Ok(Value::Null)
}
