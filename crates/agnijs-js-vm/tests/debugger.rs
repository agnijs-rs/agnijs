//! The debugger interface (`agnijs_js_debugger::Debuggee`), driven by a
//! scripted client: each pause runs the next queued handler, which inspects
//! the pause and decides how to resume.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{ConsoleLevel, RealmId, Value};
use agnijs_js_debugger::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

type Handler = Box<dyn FnMut(&mut dyn Debuggee, &PauseInfo) -> ResumeAction>;

#[derive(Default)]
struct Client {
    handlers: RefCell<VecDeque<Handler>>,
    pauses: RefCell<Vec<(String, u32, u32)>>,
    parsed: RefCell<Vec<ScriptInfo>>,
    console: RefCell<Vec<(ConsoleLevel, Vec<Value>)>>,
    exceptions: RefCell<Vec<ExceptionEvent>>,
}

impl Client {
    fn on_pause(&self, f: impl FnMut(&mut dyn Debuggee, &PauseInfo) -> ResumeAction + 'static) {
        self.handlers.borrow_mut().push_back(Box::new(f));
    }
    /// `(function, line, column)` of the innermost frame at every pause.
    fn pauses(&self) -> Vec<(String, u32, u32)> {
        self.pauses.borrow().clone()
    }
}

impl DebugClient for Client {
    fn script_parsed(&self, _d: &mut dyn Debuggee, script: &ScriptInfo) {
        self.parsed.borrow_mut().push(script.clone());
    }
    fn paused(&self, d: &mut dyn Debuggee, pause: &PauseInfo) -> ResumeAction {
        let top = &pause.frames[0];
        self.pauses.borrow_mut().push((
            top.function_name.clone(),
            top.location.line,
            top.location.column,
        ));
        let handler = self.handlers.borrow_mut().pop_front();
        match handler {
            Some(mut h) => h(d, pause),
            None => ResumeAction::Continue,
        }
    }
    fn console(&self, _d: &mut dyn Debuggee, _realm: RealmId, level: ConsoleLevel, args: &[Value]) {
        self.console.borrow_mut().push((level, args.to_vec()));
    }
    fn exception_thrown(&self, _d: &mut dyn Debuggee, e: &ExceptionEvent) {
        self.exceptions.borrow_mut().push(e.clone());
    }
}

fn debuggee() -> (Box<dyn Debuggee>, Rc<Client>, RealmId) {
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let client = Rc::new(Client::default());
    d.attach(client.clone());
    let realm = d.engine().main_realm();
    (d, client, realm)
}

fn run(d: &mut dyn Debuggee, realm: RealmId, name: &str, src: &str) -> Value {
    d.engine()
        .eval_script(realm, name, src)
        .unwrap_or_else(|e| panic!("{e}"))
}

fn at_line(url: &str, line: u32) -> BreakpointSpec {
    BreakpointSpec {
        target: BreakpointTarget::Url(url.into()),
        line,
        column: None,
        condition: None,
    }
}

fn variables(scope: &ScopeInfo) -> Vec<(String, Value)> {
    match &scope.contents {
        ScopeContents::Variables(v) => v.clone(),
        ScopeContents::Object(_) => panic!("object scope"),
    }
}

fn var(scope: &ScopeInfo, name: &str) -> Option<Value> {
    variables(scope)
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
}

const ADD: &str = "function add(a, b) {
  const sum = a + b;
  return sum;
}
globalThis.result = add(2, 3);
";

#[test]
fn a_url_breakpoint_pauses_before_its_line_with_frames_and_locals() {
    let (mut d, client, realm) = debuggee();
    let (id, resolved) = d.set_breakpoint(at_line("add.js", 2));
    assert!(resolved.is_empty(), "the script is not compiled yet");
    client.on_pause(move |_d, p| {
        assert!(matches!(&p.reason, PauseReason::Breakpoint(ids) if ids == &[id]));
        assert_eq!(p.frames.len(), 2);
        assert_eq!(p.frames[0].function_name, "add");
        assert_eq!(p.frames[0].url, "add.js");
        assert_eq!(p.frames[1].function_name, "");
        assert_eq!((p.frames[1].location.line, p.frames[1].location.column), (4, 20));
        let local = &p.frames[0].scopes[0];
        assert_eq!(local.kind, ScopeKind::Local);
        assert_eq!(var(local, "a"), Some(Value::Number(2.0)));
        assert_eq!(var(local, "b"), Some(Value::Number(3.0)));
        assert_eq!(var(local, "sum"), Some(Value::Number(5.0)));
        assert_eq!(p.frames[0].scopes.last().unwrap().kind, ScopeKind::Global);
        ResumeAction::Continue
    });
    run(&mut *d, realm, "add.js", ADD);
    assert_eq!(client.pauses(), vec![("add".to_string(), 2, 2)]);
    assert_eq!(run(&mut *d, realm, "check.js", "result"), Value::Number(5.0));
}

const STEPS: &str = "function inner(x) {
  const y = x * 2;
  return y + 1;
}
function outer() {
  debugger;
  const a = inner(1);
  const b = inner(a);
  return a + b;
}
globalThis.out = outer();
";

#[test]
fn stepping_over_into_and_out_stops_at_statement_starts() {
    let (mut d, client, realm) = debuggee();
    client.on_pause(|_d, p| {
        assert!(matches!(p.reason, PauseReason::DebuggerStatement));
        ResumeAction::StepOver
    });
    client.on_pause(|_d, p| {
        assert!(matches!(p.reason, PauseReason::Step));
        ResumeAction::StepInto
    });
    client.on_pause(|_d, p| {
        assert_eq!(p.frames.len(), 3, "inner, outer and the script");
        assert_eq!(var(&p.frames[0].scopes[0], "x"), Some(Value::Number(1.0)));
        ResumeAction::StepOut
    });
    client.on_pause(|_d, _p| ResumeAction::StepOver);
    client.on_pause(|_d, _p| ResumeAction::Continue);
    run(&mut *d, realm, "steps.js", STEPS);
    assert_eq!(
        client.pauses(),
        vec![
            ("outer".to_string(), 5, 2),
            ("outer".to_string(), 6, 2),
            ("inner".to_string(), 1, 2),
            ("outer".to_string(), 7, 2),
            ("outer".to_string(), 8, 2),
        ]
    );
    assert_eq!(run(&mut *d, realm, "check.js", "out"), Value::Number(10.0));
}

const STEPS_THROUGH_CALL: &str = "function inner(x) {
  const y = x * 2;
  return y + 1;
}
var o = { get g() {
  return 7;
} };
function outer() {
  debugger;
  const a = inner.call(null, 1);
  const b = inner.apply(null, [a]);
  const c = o.g;
  return a + b + c;
}
globalThis.out = outer();
";

/// Functions reached through `Function.prototype.call` and `apply`, and
/// getters, run in the caller's run loop (TASK-100.19): stepping into and
/// out of them stops where it does for a direct call.
#[test]
fn stepping_into_call_apply_and_a_getter_stops_in_the_target() {
    let (mut d, client, realm) = debuggee();
    client.on_pause(|_d, p| {
        assert!(matches!(p.reason, PauseReason::DebuggerStatement));
        ResumeAction::StepOver
    });
    client.on_pause(|_d, _p| ResumeAction::StepInto);
    client.on_pause(|_d, p| {
        assert_eq!(p.frames.len(), 3, "inner, outer and the script");
        assert_eq!(var(&p.frames[0].scopes[0], "x"), Some(Value::Number(1.0)));
        ResumeAction::StepOut
    });
    client.on_pause(|_d, _p| ResumeAction::StepInto);
    client.on_pause(|_d, p| {
        assert_eq!(p.frames.len(), 3, "inner, outer and the script");
        assert_eq!(var(&p.frames[0].scopes[0], "x"), Some(Value::Number(3.0)));
        ResumeAction::StepOver
    });
    client.on_pause(|_d, _p| ResumeAction::StepOver);
    client.on_pause(|_d, _p| ResumeAction::StepInto);
    client.on_pause(|_d, p| {
        assert_eq!(p.frames.len(), 3, "the getter, outer and the script");
        ResumeAction::StepOut
    });
    client.on_pause(|_d, _p| ResumeAction::Continue);
    run(&mut *d, realm, "steps.js", STEPS_THROUGH_CALL);
    assert_eq!(
        client.pauses(),
        vec![
            ("outer".to_string(), 8, 2),
            ("outer".to_string(), 9, 2),
            ("inner".to_string(), 1, 2),
            ("outer".to_string(), 10, 2),
            ("inner".to_string(), 1, 2),
            ("inner".to_string(), 2, 2),
            ("outer".to_string(), 11, 2),
            (String::new(), 5, 2),
            ("outer".to_string(), 12, 2),
        ]
    );
    assert_eq!(run(&mut *d, realm, "check.js", "out"), Value::Number(17.0));
}

/// Precompiled code carries its debug info in the container and decodes it
/// when the debugger first needs it: breakpoints resolve, pauses report the
/// statement's position, locals are named and stepping stops at statements.
#[test]
fn a_precompiled_script_pauses_steps_and_shows_locals_like_source() {
    let (mut d, client, realm) = debuggee();
    let bytes = d.engine().compile_script(realm, "add.js", ADD).unwrap();
    let (id, resolved) = d.set_breakpoint(at_line("add.js", 2));
    assert!(resolved.is_empty());
    client.on_pause(move |_d, p| {
        assert!(matches!(&p.reason, PauseReason::Breakpoint(ids) if ids == &[id]));
        assert_eq!(p.frames[0].function_name, "add");
        assert_eq!((p.frames[1].location.line, p.frames[1].location.column), (4, 20));
        let local = &p.frames[0].scopes[0];
        assert_eq!(var(local, "a"), Some(Value::Number(2.0)));
        assert_eq!(var(local, "sum"), Some(Value::Number(5.0)));
        ResumeAction::Continue
    });
    d.engine().eval_bytecode(realm, &bytes).unwrap();
    assert_eq!(client.pauses(), vec![("add".to_string(), 2, 2)]);
    assert_eq!(run(&mut *d, realm, "check.js", "result"), Value::Number(5.0));
    let script = d.scripts().iter().find(|s| s.url == "add.js").unwrap().id;
    let lines: Vec<u32> = d
        .breakable_locations(Location { script, line: 0, column: 0 }, None)
        .iter()
        .map(|l| l.line)
        .collect();
    assert_eq!(lines, vec![0, 1, 2], "the same lines as a source run");

    // Stepping through precompiled code stops where the source run does.
    let (mut d, client, realm) = debuggee();
    let bytes = d.engine().compile_script(realm, "steps.js", STEPS).unwrap();
    client.on_pause(|_d, _p| ResumeAction::StepOver);
    client.on_pause(|_d, _p| ResumeAction::StepInto);
    client.on_pause(|_d, p| {
        assert_eq!(var(&p.frames[0].scopes[0], "x"), Some(Value::Number(1.0)));
        ResumeAction::StepOut
    });
    client.on_pause(|_d, _p| ResumeAction::StepOver);
    client.on_pause(|_d, _p| ResumeAction::Continue);
    d.engine().eval_bytecode(realm, &bytes).unwrap();
    assert_eq!(
        client.pauses(),
        vec![
            ("outer".to_string(), 5, 2),
            ("outer".to_string(), 6, 2),
            ("inner".to_string(), 1, 2),
            ("outer".to_string(), 7, 2),
            ("outer".to_string(), 8, 2),
        ]
    );
}

#[test]
fn a_step_that_runs_off_the_end_of_the_script_does_not_leak_into_the_next() {
    let (mut d, client, realm) = debuggee();
    client.on_pause(|_d, _p| ResumeAction::StepOver);
    run(&mut *d, realm, "one.js", "debugger;\n");
    run(&mut *d, realm, "two.js", "let z = 1;\nz + 1;\n");
    assert_eq!(client.pauses(), vec![(String::new(), 0, 0)]);
}

const COUNTER: &str = "let total = 100;
function makeCounter(step) {
  let count = 0;
  return function tick(times) {
    for (let i = 0; i < times; i++) {
      count += step;
    }
    debugger;
    return count;
  };
}
const tick = makeCounter(5);
globalThis.first = tick(2);
";

#[test]
fn scopes_show_block_locals_closures_script_lexicals_and_the_global_object() {
    let (mut d, client, realm) = debuggee();
    client.on_pause(|d, p| {
        let f = &p.frames[0];
        assert_eq!(f.function_name, "tick");
        assert_eq!(var(&f.scopes[0], "times"), Some(Value::Number(2.0)));
        assert_eq!(var(&f.scopes[0], "i"), None, "the loop variable is out of scope");
        let closure = f
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::Closure)
            .expect("a closure scope");
        assert_eq!(var(closure, "count"), Some(Value::Number(10.0)));
        assert_eq!(var(closure, "step"), Some(Value::Number(5.0)));
        let script = f
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::Script)
            .expect("a script scope");
        assert_eq!(var(script, "total"), Some(Value::Number(100.0)));
        let ScopeContents::Object(global) = &f.scopes.last().unwrap().contents else {
            panic!("the global scope is an object");
        };
        let props = d.properties(global).unwrap();
        assert!(props
            .properties
            .iter()
            .any(|p| p.name == PropertyName::String("makeCounter".into())));
        ResumeAction::Continue
    });
    run(&mut *d, realm, "counter.js", COUNTER);
    assert_eq!(client.pauses().len(), 1);
}

#[test]
fn evaluate_on_a_frame_reads_and_writes_its_locals_and_closures() {
    let (mut d, client, realm) = debuggee();
    client.on_pause(|d, _p| {
        assert_eq!(
            d.evaluate_on_frame(0, "count * 100 + times").unwrap(),
            Value::Number(1002.0)
        );
        // Writes reach the closure variable and the register-held parameter.
        d.evaluate_on_frame(0, "count = 41; times = 7").unwrap();
        assert_eq!(d.evaluate_on_frame(0, "times").unwrap(), Value::Number(7.0));
        // The caller's frame sees its own bindings.
        assert_eq!(d.evaluate_on_frame(1, "typeof tick").unwrap(), Value::String("function".into()));
        let err = d.evaluate_on_frame(0, "nope.x").unwrap_err();
        assert!(err.to_string().contains("ReferenceError"), "{err}");
        assert!(matches!(d.evaluate_on_frame(9, "1"), Err(DebugError::NoSuchFrame)));
        ResumeAction::Continue
    });
    run(&mut *d, realm, "counter.js", COUNTER);
    assert_eq!(run(&mut *d, realm, "check.js", "first"), Value::Number(41.0));
    assert!(matches!(d.evaluate_on_frame(0, "1"), Err(DebugError::NotPaused)));
}

#[test]
fn a_conditional_breakpoint_pauses_only_when_its_condition_holds() {
    let (mut d, client, realm) = debuggee();
    let mut spec = at_line("loop.js", 2);
    spec.condition = Some("i === 3".into());
    d.set_breakpoint(spec);
    client.on_pause(|d, _p| {
        assert_eq!(d.evaluate_on_frame(0, "i").unwrap(), Value::Number(3.0));
        ResumeAction::Continue
    });
    run(
        &mut *d,
        realm,
        "loop.js",
        "let s = 0;\nfor (let i = 0; i < 5; i++) {\n  s += i;\n}\n",
    );
    assert_eq!(client.pauses(), vec![(String::new(), 2, 2)]);
}

#[test]
fn breakpoints_resolve_to_the_next_statement_and_can_be_removed() {
    let (mut d, client, realm) = debuggee();
    run(&mut *d, realm, "lib.js", "function f() {\n\n  return 1;\n}\n");
    let script = d.scripts().iter().find(|s| s.url == "lib.js").unwrap().id;
    let (id, at) = d.set_breakpoint(BreakpointSpec {
        target: BreakpointTarget::Script(script),
        line: 1,
        column: None,
        condition: None,
    });
    assert_eq!(at, vec![Location { script, line: 2, column: 2 }]);
    run(&mut *d, realm, "a.js", "f();");
    assert!(d.remove_breakpoint(id));
    assert!(!d.remove_breakpoint(id));
    run(&mut *d, realm, "b.js", "f();");
    assert_eq!(client.pauses(), vec![("f".to_string(), 2, 2)]);
    let lines: Vec<u32> = d
        .breakable_locations(Location { script, line: 0, column: 0 }, None)
        .iter()
        .map(|l| l.line)
        .collect();
    assert_eq!(lines, vec![0, 2]);
}

#[test]
fn scripts_are_listed_with_their_source_including_those_run_before_attaching() {
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    run(&mut *d, realm, "early.js", "var early = 1;\n//# sourceMappingURL=early.js.map\n");
    let client = Rc::new(Client::default());
    d.attach(client.clone());
    run(&mut *d, realm, "late.js", "eval('1 + 1');\n");
    let urls: Vec<String> = client.parsed.borrow().iter().map(|s| s.url.clone()).collect();
    assert_eq!(urls, vec!["early.js", "late.js", "eval"]);
    let early = d.scripts().into_iter().find(|s| s.url == "early.js").unwrap();
    assert_eq!(early.source_map_url.as_deref(), Some("early.js.map"));
    assert_eq!(early.kind, ScriptKind::Script);
    assert_eq!((early.end_line, early.end_column), (2, 0));
    assert_eq!(
        d.script_source(early.id).as_deref(),
        Some("var early = 1;\n//# sourceMappingURL=early.js.map\n")
    );
    let eval = d.scripts().into_iter().find(|s| s.url == "eval").unwrap();
    assert_eq!(eval.kind, ScriptKind::Eval);
}

// ---- fused compare-and-branch (TASK-100.18) --------------------------------------------------

const FUSED: &str = "globalThis.picked = pick(1, 2);
for (var i = 0; i < 2; i++) {
  globalThis.picked += i;
}
function pick(a, b) {
  debugger;
  if (a < b && b < 10) {
    return 1;
  }
  return 2;
}
";

#[test]
fn stepping_over_a_fused_condition_pauses_on_each_statement_line() {
    // An `if` whose test is one compare-and-branch instruction, and a `for`
    // loop whose test and update are: the statement starts and lines are
    // those of the source, and each step lands on the next statement.
    let (mut d, client, realm) = debuggee();
    for _ in 0..6 {
        client.on_pause(|_d, _p| ResumeAction::StepOver);
    }
    run(&mut *d, realm, "fused.js", FUSED);
    // Stepping over `return 1` lands on the caller's next statement (the
    // assignment on line 0 is already under way), then the loop body twice,
    // then the script's end (the hoisted declaration's start, line 4).
    assert_eq!(
        client.pauses(),
        vec![
            ("pick".to_string(), 5, 2),
            ("pick".to_string(), 6, 2),
            ("pick".to_string(), 7, 4),
            (String::new(), 1, 0),
            (String::new(), 2, 2),
            (String::new(), 2, 2),
            (String::new(), 4, 0),
        ]
    );
    assert_eq!(run(&mut *d, realm, "check.js", "picked"), Value::Number(2.0));
    let script = d.scripts().iter().find(|s| s.url == "fused.js").unwrap().id;
    let lines: Vec<u32> = d
        .breakable_locations(Location { script, line: 0, column: 0 }, None)
        .iter()
        .map(|l| l.line)
        .collect();
    // Line 4 is the function declaration's own (hoisted) start, as for any
    // declaration; the fused `if` and `for` tests keep lines 1, 2, 6 and 7.
    assert_eq!(lines, vec![0, 1, 2, 4, 5, 6, 7, 9]);
}

#[test]
fn a_breakpoint_on_a_loop_whose_test_is_fused_pauses_each_iteration_with_the_locals() {
    let (mut d, client, realm) = debuggee();
    d.set_breakpoint(at_line("loop.js", 2));
    let seen = Rc::new(RefCell::new(Vec::new()));
    for _ in 0..3 {
        let s = seen.clone();
        client.on_pause(move |d, _p| {
            s.borrow_mut().push(d.evaluate_on_frame(0, "i * 10 + s").unwrap());
            ResumeAction::Continue
        });
    }
    run(
        &mut *d,
        realm,
        "loop.js",
        "function sum(n) {\n  for (var i = 0, s = 0; i < n; i++) {\n    s += i;\n  }\n  return s;\n}\nglobalThis.total = sum(3);\n",
    );
    assert_eq!(
        *seen.borrow(),
        vec![Value::Number(0.0), Value::Number(10.0), Value::Number(21.0)]
    );
    assert_eq!(client.pauses().len(), 3);
    assert_eq!(run(&mut *d, realm, "check.js", "total"), Value::Number(3.0));
}

#[test]
fn an_exception_thrown_inside_a_fused_comparison_is_reported_at_the_comparison() {
    let (mut d, client, realm) = debuggee();
    d.set_pause_on_exceptions(PauseOnExceptions::All);
    client.on_pause(|d, p| {
        assert!(matches!(&p.reason, PauseReason::Exception { .. }));
        // The throw is in valueOf; the frame below is paused inside the
        // fused compare-and-branch, reported at its statement (the `if`),
        // with its locals as they were before the comparison.
        let caller = &p.frames[1];
        assert_eq!(caller.function_name, "cmp");
        assert_eq!((caller.location.line, caller.location.column), (2, 2));
        assert_eq!(d.evaluate_on_frame(1, "s").unwrap(), Value::Number(10.0));
        ResumeAction::Continue
    });
    let src = "function cmp(a, b) {\n  var s = 10;\n  if (a < b) {\n    s = 1;\n  }\n  return s;\n}\ntry { cmp({ valueOf() { throw new TypeError('bad'); } }, 2); } catch (e) {}\n";
    run(&mut *d, realm, "cmp.js", src);
    // The pause itself is at the `throw` statement inside the (anonymous)
    // valueOf method.
    assert_eq!(client.pauses(), vec![(String::new(), 7, 24)]);
}

const THROWS: &str = "function fail(msg) {
  throw new TypeError(msg);
}
try {
  fail('caught');
} catch (e) {}
fail('escapes');
";

fn exception_pauses(mode: PauseOnExceptions) -> (Vec<(bool, u32)>, Rc<Client>) {
    let (mut d, client, realm) = debuggee();
    d.set_pause_on_exceptions(mode);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let s = seen.clone();
    for _ in 0..2 {
        let s = s.clone();
        client.on_pause(move |d, p| {
            let PauseReason::Exception { value, uncaught } = &p.reason else {
                panic!("not an exception pause");
            };
            assert_eq!(d.describe(value).subtype, Some(Subtype::Error));
            s.borrow_mut().push((*uncaught, p.frames[0].location.line));
            ResumeAction::Continue
        });
    }
    assert!(d.engine().eval_script(realm, "throws.js", THROWS).is_err());
    let out = seen.borrow().clone();
    (out, client)
}

#[test]
fn pausing_on_exceptions_distinguishes_caught_from_uncaught() {
    assert_eq!(exception_pauses(PauseOnExceptions::None).0, vec![]);
    assert_eq!(exception_pauses(PauseOnExceptions::Uncaught).0, vec![(true, 1)]);
    assert_eq!(
        exception_pauses(PauseOnExceptions::All).0,
        vec![(false, 1), (true, 1)]
    );
}

#[test]
fn an_uncaught_exception_is_reported_with_the_stack_where_it_was_thrown() {
    let (_, client) = exception_pauses(PauseOnExceptions::None);
    let events = client.exceptions.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].text, "Uncaught TypeError: escapes");
    let stack: Vec<(String, u32, u32)> = events[0]
        .stack
        .iter()
        .map(|f| (f.function_name.clone(), f.line, f.column))
        .collect();
    assert_eq!(
        stack,
        vec![("fail".to_string(), 1, 2), (String::new(), 6, 0)]
    );
    assert_eq!(events[0].stack[0].url, "throws.js");
}

#[test]
fn syntax_errors_are_reported_as_exceptions() {
    let (mut d, client, realm) = debuggee();
    assert!(d.engine().eval_script(realm, "bad.js", "let = = 1;").is_err());
    let events = client.exceptions.borrow();
    assert_eq!(events.len(), 1);
    assert!(events[0].text.starts_with("Uncaught SyntaxError"), "{}", events[0].text);
}

#[test]
fn console_calls_reach_the_client_with_their_values() {
    let (mut d, client, realm) = debuggee();
    run(&mut *d, realm, "log.js", "console.warn('n', 42, {a: 1});");
    let calls = client.console.borrow();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, ConsoleLevel::Warn);
    assert_eq!(calls[0].1[0], Value::String("n".into()));
    assert_eq!(calls[0].1[1], Value::Number(42.0));
    let obj = calls[0].1[2].clone();
    drop(calls);
    let desc = d.describe(&obj);
    assert_eq!((desc.kind, desc.class_name.as_deref()), (ValueKind::Object, Some("Object")));
    let props = d.properties(&obj).unwrap();
    assert_eq!(props.properties.len(), 1);
    assert_eq!(props.properties[0].name, PropertyName::String("a".into()));
    assert_eq!(props.properties[0].value, Some(Value::Number(1.0)));
}

const ASYNC: &str = "async function load() {
  await null;
  debugger;
}
function start() {
  return load();
}
start();
";

/// Each async segment: its description and the (function, line) frames.
type AsyncSegments = Vec<(String, Vec<(String, u32)>)>;

fn async_stack_lines(depth: u32) -> Option<AsyncSegments> {
    let (mut d, client, realm) = debuggee();
    d.set_async_stack_depth(depth);
    let seen = Rc::new(RefCell::new(None));
    let s = seen.clone();
    client.on_pause(move |_d, p| {
        assert_eq!(p.frames[0].function_name, "load");
        let mut chain = Vec::new();
        let mut cur = p.async_stack.clone();
        while let Some(seg) = cur {
            let frames = seg
                .frames
                .iter()
                .map(|f| (f.function_name.clone(), f.line))
                .collect();
            chain.push((seg.description.clone(), frames));
            cur = seg.parent.clone();
        }
        *s.borrow_mut() = Some(chain);
        ResumeAction::Continue
    });
    run(&mut *d, realm, "async.js", ASYNC);
    d.engine().run_jobs(realm).unwrap();
    assert_eq!(client.pauses().len(), 1);
    let out = seen.borrow().clone();
    out
}

#[test]
fn a_pause_after_await_carries_the_async_stack_of_the_caller() {
    assert_eq!(
        async_stack_lines(32),
        Some(vec![(
            "await".to_string(),
            vec![("start".to_string(), 5), (String::new(), 7)]
        )])
    );
    assert_eq!(async_stack_lines(0), Some(vec![]));
}

const STEP_ASYNC: &str = "async function work() {
  debugger;
  const a = await one();
  const b = await a;
  return a + b;
}
function one() { return Promise.resolve(1); }
function* gen() {
  debugger;
  yield 1;
  yield 2;
}
work().then(function (v) { globalThis.done = v; });
const it = gen(); it.next(); it.next(); it.next();
";

/// Stepping over an `await` or a `yield` leaves for the caller, as it did
/// before TASK-100.25 replaced the await reaction closures by internal
/// records, and a breakpoint in the resumed body pauses inside the promise
/// job with the `await` segment on the async stack.
#[test]
fn stepping_over_await_and_yield_and_pausing_after_the_resumption() {
    let (mut d, client, realm) = debuggee();
    d.set_async_stack_depth(32);
    d.set_breakpoint(at_line("step-async.js", 4));
    for _ in 0..8 {
        client.on_pause(|_d, _p| ResumeAction::StepOver);
    }
    let seen = Rc::new(RefCell::new(None));
    let s = seen.clone();
    client.on_pause(move |_d, p| {
        assert!(matches!(&p.reason, PauseReason::Breakpoint(_)));
        let local = &p.frames[0].scopes[0];
        *s.borrow_mut() = Some((
            var(local, "a"),
            var(local, "b"),
            p.async_stack.as_ref().map(|a| a.description.clone()),
        ));
        ResumeAction::Continue
    });
    run(&mut *d, realm, "step-async.js", STEP_ASYNC);
    d.engine().run_jobs(realm).unwrap();
    assert_eq!(
        client.pauses(),
        vec![
            ("work".to_string(), 1, 2),
            ("work".to_string(), 2, 2),
            (String::new(), 13, 0),
            (String::new(), 13, 18),
            ("gen".to_string(), 8, 2),
            ("gen".to_string(), 9, 2),
            (String::new(), 13, 29),
            (String::new(), 13, 40),
            ("work".to_string(), 4, 2),
        ]
    );
    assert_eq!(
        *seen.borrow(),
        Some((
            Some(Value::Number(1.0)),
            Some(Value::Number(1.0)),
            Some("await".to_string())
        ))
    );
    assert_eq!(run(&mut *d, realm, "check.js", "done"), Value::Number(2.0));
}

#[test]
fn script_parsed_reports_functions_and_detaching_stops_pausing() {
    let (mut d, client, realm) = debuggee();
    run(&mut *d, realm, "fn.js", "new Function('a', 'return a')(1);");
    assert_eq!(client.parsed.borrow().last().unwrap().kind, ScriptKind::Function);
    d.detach();
    assert!(!d.is_attached());
    run(&mut *d, realm, "quiet.js", "debugger; 1;");
    assert!(client.pauses().is_empty());
}

/// Detaches from inside its first callback, as an embedder's client may.
struct DetachingClient {
    calls: RefCell<u32>,
}

impl DebugClient for DetachingClient {
    fn script_parsed(&self, d: &mut dyn Debuggee, _script: &ScriptInfo) {
        *self.calls.borrow_mut() += 1;
        d.detach();
    }
    fn paused(&self, d: &mut dyn Debuggee, _pause: &PauseInfo) -> ResumeAction {
        *self.calls.borrow_mut() += 1;
        d.detach();
        ResumeAction::Continue
    }
}

#[test]
fn a_client_detaching_inside_a_callback_does_not_panic_the_engine() {
    // While attach replays the scripts the engine already holds: the
    // client detaches on the first, and the replay must stop quietly.
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    run(&mut *d, realm, "a.js", "1");
    run(&mut *d, realm, "b.js", "2");
    let client = Rc::new(DetachingClient { calls: RefCell::new(0) });
    d.attach(client.clone());
    assert!(!d.is_attached());
    assert_eq!(*client.calls.borrow(), 1);
    // While paused.
    let fresh = Rc::new(DetachingClient { calls: RefCell::new(0) });
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    let realm = d.engine().main_realm();
    d.attach(fresh.clone());
    let v = run(&mut *d, realm, "", "debugger; [1, 2].map(x => x * 2).join()");
    assert_eq!(v, Value::string("2,4"));
    assert!(!d.is_attached());
}
