//! The debugger interface: what a debugger front end (the CDP adapter in
//! `gable-js-cdp`, or a host's own tooling) needs from an engine, in
//! engine-neutral terms.
//!
//! - [`Debuggee`] is implemented by an engine that can be debugged. It
//!   extends nothing: it hands out the engine's [`Engine`] interface through
//!   [`Debuggee::engine`] so a front end can evaluate code and read values
//!   with the calls it already knows.
//! - [`DebugClient`] is implemented by the front end and attached to the
//!   debuggee. The engine calls it when a script is compiled, when execution
//!   pauses, when `console` is called and when an exception escapes to the
//!   host.
//!
//! Pausing is synchronous and nested: when a breakpoint is hit the engine
//! calls [`DebugClient::paused`] from inside its interpreter loop, the client
//! inspects frames, evaluates code and finally returns a [`ResumeAction`].
//! While the client is inside `paused` no further pause happens (code it
//! evaluates runs to completion).
//!
//! Positions are 0-based lines and 0-based columns counted in UTF-16 code
//! units, as the Chrome DevTools Protocol counts them.
//!
//! See `docs/js-engine-spec.md` section 9 and ADR 0017.

pub mod source_map;

pub use gable_engine_api::{ConsoleLevel, Engine, Error, RealmId, Value};
pub use source_map::{OriginalPosition, SourceMap};

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Identifies one compiled script, module, `eval` body or `Function` body
/// for the life of the attached debugger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScriptId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptKind {
    Script,
    Module,
    /// The body of a direct or indirect `eval`.
    Eval,
    /// The body of a `Function` (or generator/async) constructor call.
    Function,
}

#[derive(Clone, Debug)]
pub struct ScriptInfo {
    pub id: ScriptId,
    /// The name given at evaluation (script name or module specifier).
    pub url: String,
    pub kind: ScriptKind,
    pub realm: RealmId,
    /// The last line (0-based) and the length of that line in UTF-16 units.
    pub end_line: u32,
    pub end_column: u32,
    /// The `//# sourceMappingURL=` comment's value, if the source has one.
    pub source_map_url: Option<String>,
}

/// A position in a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub script: ScriptId,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BreakpointId(pub u32);

/// Where a breakpoint goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BreakpointTarget {
    /// One compiled script.
    Script(ScriptId),
    /// Every script with this URL, including scripts compiled later.
    Url(String),
}

#[derive(Clone, Debug)]
pub struct BreakpointSpec {
    pub target: BreakpointTarget,
    pub line: u32,
    /// `None`: the first breakable position on or after the line start.
    pub column: Option<u32>,
    /// An expression evaluated in the paused frame; the breakpoint only
    /// pauses when it is truthy (an exception counts as false).
    pub condition: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseOnExceptions {
    None,
    /// Only exceptions no JavaScript handler on the stack will catch.
    Uncaught,
    All,
}

/// What the client wants after a pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAction {
    Continue,
    /// Stop at the next statement, entering calls.
    StepInto,
    /// Stop at the next statement in this frame or a caller.
    StepOver,
    /// Stop at the next statement in a caller.
    StepOut,
}

#[derive(Clone, Debug)]
pub enum PauseReason {
    Breakpoint(Vec<BreakpointId>),
    Step,
    DebuggerStatement,
    /// An exception was thrown; `uncaught` when no JavaScript handler on the
    /// stack will catch it.
    Exception { value: Value, uncaught: bool },
    /// [`PauseRequester::request`] was called.
    Requested,
}

/// Everything about one pause.
#[derive(Clone, Debug)]
pub struct PauseInfo {
    pub reason: PauseReason,
    /// Innermost first.
    pub frames: Vec<FrameInfo>,
    /// Where the asynchronous operation that led here was scheduled, when
    /// async stack capture is on ([`Debuggee::set_async_stack_depth`]).
    pub async_stack: Option<Rc<AsyncStackTrace>>,
}

/// A JavaScript frame on the paused stack.
#[derive(Clone, Debug)]
pub struct FrameInfo {
    /// 0 is the innermost frame; pass it to [`Debuggee::evaluate_on_frame`].
    pub index: usize,
    /// Empty for script, module and eval code.
    pub function_name: String,
    pub location: Location,
    pub url: String,
    pub this: Value,
    /// Innermost first, ending with the global scope.
    pub scopes: Vec<ScopeInfo>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// The frame's own bindings (registers and the environments it pushed).
    Local,
    /// An enclosing function's or block's captured bindings.
    Closure,
    Catch,
    Block,
    With,
    /// Top-level `let`, `const` and `class` declarations of scripts.
    Script,
    Module,
    Eval,
    Global,
}

#[derive(Clone, Debug)]
pub enum ScopeContents {
    /// A declarative scope, materialised as name/value pairs (bindings
    /// still in their temporal dead zone are left out).
    Variables(Vec<(String, Value)>),
    /// An object scope (`with`, the global object): read its properties.
    Object(Value),
}

#[derive(Clone, Debug)]
pub struct ScopeInfo {
    pub kind: ScopeKind,
    pub contents: ScopeContents,
}

/// One frame of a captured stack (exceptions, async stacks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFrameInfo {
    pub function_name: String,
    /// `None` when the code was compiled before the debugger knew about it.
    pub script: Option<ScriptId>,
    pub url: String,
    pub line: u32,
    pub column: u32,
}

/// A segment of an asynchronous stack: the frames that were on the stack
/// when a promise reaction (a `then` callback or the continuation of an
/// `await`) was registered, and the segment before that.
#[derive(Clone, Debug)]
pub struct AsyncStackTrace {
    /// `"await"` or `"Promise.then"`.
    pub description: String,
    /// Innermost first.
    pub frames: Vec<StackFrameInfo>,
    pub parent: Option<Rc<AsyncStackTrace>>,
}

/// An exception that escaped from the engine to the host.
#[derive(Clone, Debug)]
pub struct ExceptionEvent {
    pub value: Value,
    /// `Name: message`, as an uncaught-error report would print it.
    pub text: String,
    /// Innermost first.
    pub stack: Vec<StackFrameInfo>,
}

/// The `typeof`-level kind of a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    BigInt,
    Symbol,
    Object,
    Function,
}

/// A finer classification of objects (the CDP `subtype`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subtype {
    Array,
    Error,
    Promise,
    RegExp,
    Date,
    Map,
    Set,
    WeakMap,
    WeakSet,
    WeakRef,
    Proxy,
    TypedArray,
    ArrayBuffer,
    DataView,
    Generator,
    Iterator,
    Module,
}

impl Subtype {
    /// The CDP spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Subtype::Array => "array",
            Subtype::Error => "error",
            Subtype::Promise => "promise",
            Subtype::RegExp => "regexp",
            Subtype::Date => "date",
            Subtype::Map => "map",
            Subtype::Set => "set",
            Subtype::WeakMap => "weakmap",
            Subtype::WeakSet => "weakset",
            Subtype::WeakRef => "weakref",
            Subtype::Proxy => "proxy",
            Subtype::TypedArray => "typedarray",
            Subtype::ArrayBuffer => "arraybuffer",
            Subtype::DataView => "dataview",
            Subtype::Generator => "generator",
            Subtype::Iterator => "iterator",
            Subtype::Module => "module",
        }
    }
}

/// How a value is shown, computed without running JavaScript.
#[derive(Clone, Debug)]
pub struct ValueDescription {
    pub kind: ValueKind,
    pub subtype: Option<Subtype>,
    /// The constructor name for objects (`"Object"`, `"Array"`, `"Foo"`).
    pub class_name: Option<String>,
    /// A one-line description (`"Array(3)"`, `"Error: boom\n    at ..."`,
    /// the source of a function, a primitive's display string).
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropertyName {
    String(String),
    Symbol(Value),
}

/// An own property, read without running getters or proxy traps.
#[derive(Clone, Debug)]
pub struct PropertyInfo {
    pub name: PropertyName,
    /// Set for data properties.
    pub value: Option<Value>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ObjectProperties {
    pub properties: Vec<PropertyInfo>,
    /// Internal slots worth showing: `[[Prototype]]`, `[[PrimitiveValue]]`,
    /// `[[PromiseState]]`, `[[PromiseResult]]`, `[[Target]]`, `[[Handler]]`,
    /// `[[BoundThis]]`, `[[TargetFunction]]`, `[[Entries]]` and the like.
    pub internal: Vec<(String, Value)>,
}

/// Asks a running engine to pause at the next statement. `Send`, so a
/// transport thread can call it while the engine thread runs JavaScript.
#[derive(Clone, Debug, Default)]
pub struct PauseRequester(Arc<AtomicBool>);

impl PauseRequester {
    pub fn new() -> PauseRequester {
        PauseRequester::default()
    }
    pub fn request(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    /// Take a pending request (engine side).
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::Relaxed)
    }
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Failures of debugger calls.
#[derive(Clone, Debug)]
pub enum DebugError {
    /// The call needs a paused engine.
    NotPaused,
    /// No such frame on the paused stack.
    NoSuchFrame,
    /// The value is not an object.
    NotAnObject,
    /// The script id is unknown.
    NoSuchScript,
    /// An engine error (a JavaScript exception from evaluation, a foreign
    /// handle, an interrupt).
    Engine(Error),
}

impl From<Error> for DebugError {
    fn from(e: Error) -> DebugError {
        DebugError::Engine(e)
    }
}

impl std::fmt::Display for DebugError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugError::NotPaused => write!(f, "the engine is not paused"),
            DebugError::NoSuchFrame => write!(f, "no such frame"),
            DebugError::NotAnObject => write!(f, "the value is not an object"),
            DebugError::NoSuchScript => write!(f, "no such script"),
            DebugError::Engine(e) => write!(f, "{e}"),
        }
    }
}

/// An engine that can be debugged.
pub trait Debuggee {
    /// The engine interface, for evaluation and value access.
    fn engine(&mut self) -> &mut dyn Engine;

    /// Attach a client, replacing any previous one. Scripts compiled before
    /// the attach that the engine still knows are listed by [`scripts`].
    ///
    /// [`scripts`]: Debuggee::scripts
    fn attach(&mut self, client: Rc<dyn DebugClient>);
    /// Detach the client, clearing breakpoints and stepping.
    fn detach(&mut self);
    fn is_attached(&self) -> bool;

    /// Every script the debugger knows, oldest first.
    fn scripts(&self) -> Vec<ScriptInfo>;
    fn script_source(&self, script: ScriptId) -> Option<String>;

    /// Set a breakpoint; returns its id and the positions it resolved to
    /// (empty for a URL breakpoint whose script is not compiled yet).
    fn set_breakpoint(&mut self, spec: BreakpointSpec) -> (BreakpointId, Vec<Location>);
    fn remove_breakpoint(&mut self, id: BreakpointId) -> bool;
    /// Breakable positions (statement starts) in `[start, end)` of a script.
    fn breakable_locations(&self, start: Location, end: Option<Location>) -> Vec<Location>;
    /// Ignore every breakpoint while false (stepping still works).
    fn set_breakpoints_active(&mut self, active: bool);
    fn set_pause_on_exceptions(&mut self, mode: PauseOnExceptions);
    /// 0 turns async stack capture off; otherwise the frame limit per
    /// segment.
    fn set_async_stack_depth(&mut self, depth: u32);
    /// A handle that asks the engine to pause from any thread.
    fn pause_requester(&self) -> PauseRequester;

    /// Describe a value without running JavaScript.
    fn describe(&mut self, value: &Value) -> ValueDescription;
    /// Own properties and internal slots of an object, without running
    /// getters or proxy traps.
    fn properties(&mut self, object: &Value) -> Result<ObjectProperties, DebugError>;

    /// While paused: evaluate `expression` as if by a direct `eval` in frame
    /// `frame` (0 innermost). Assignments to the frame's bindings stick.
    fn evaluate_on_frame(&mut self, frame: usize, expression: &str) -> Result<Value, DebugError>;
    /// While paused: the stack, as given to [`DebugClient::paused`].
    fn paused_frames(&mut self) -> Result<Vec<FrameInfo>, DebugError>;
}

/// The front end's side: the engine calls these. Every method takes `&self`
/// and receives the debuggee, so a client keeps its own state behind
/// interior mutability and must not hold a borrow across a call into the
/// debuggee (which can call back into the client).
pub trait DebugClient {
    /// A script, module, `eval` body or `Function` body was compiled.
    fn script_parsed(&self, debuggee: &mut dyn Debuggee, script: &ScriptInfo) {
        let _ = (debuggee, script);
    }
    /// Execution paused. Return how to continue.
    fn paused(&self, debuggee: &mut dyn Debuggee, pause: &PauseInfo) -> ResumeAction;
    /// A `console` call, with the raw argument values.
    fn console(
        &self,
        debuggee: &mut dyn Debuggee,
        realm: RealmId,
        level: ConsoleLevel,
        args: &[Value],
    ) {
        let _ = (debuggee, realm, level, args);
    }
    /// An exception escaped from the engine to the host.
    fn exception_thrown(&self, debuggee: &mut dyn Debuggee, exception: &ExceptionEvent) {
        let _ = (debuggee, exception);
    }
}

/// Find the `//# sourceMappingURL=` (or legacy `//@`) comment of a source.
pub fn source_mapping_url(source: &str) -> Option<String> {
    for line in source.lines().rev() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        for prefix in ["//# sourceMappingURL=", "//@ sourceMappingURL="] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let url = rest.trim();
                if !url.is_empty() && !url.contains(char::is_whitespace) {
                    return Some(url.to_string());
                }
            }
        }
        // Only trailing comments count.
        if !t.starts_with("//") {
            return None;
        }
    }
    None
}
