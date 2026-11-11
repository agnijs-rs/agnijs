//! Stack traces (TASK-103.17): `Error.prototype.stack` in the V8 format,
//! `Error.stackTraceLimit`, `Error.captureStackTrace`,
//! `Error.prepareStackTrace` with call sites, async frames across `await`,
//! and host-supplied source maps. The expected strings are what V8 (Node)
//! prints for the same source, written out by hand, except where a comment
//! says otherwise.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value as ApiValue};
use agnijs_js_vm::Vm;

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm
}

fn eval_str(vm: &mut Vm, name: &str, src: &str) -> String {
    let realm = vm.main_realm();
    match vm.eval_script(realm, name, src) {
        Ok(ApiValue::String(s)) => s.to_string_lossy(),
        Ok(other) => panic!("{src}\n=> {other:?}"),
        Err(e) => panic!("{src}\n=> {e}"),
    }
}

/// Run `src`, drain the job queue, then evaluate `result`.
fn run_str(vm: &mut Vm, name: &str, src: &str, result: &str) -> String {
    let realm = vm.main_realm();
    if let Err(e) = vm.eval_script(realm, name, src) {
        panic!("{src}\n=> {e}");
    }
    vm.run_jobs(realm).unwrap();
    eval_str(vm, "result.js", result)
}

#[test]
fn error_stack_lists_frames_innermost_first_in_the_v8_format() {
    let mut vm = vm();
    let stack = eval_str(
        &mut vm,
        "t.js",
        "function inner() {\n  return new Error('boom');\n}\nfunction outer() { return inner(); }\nouter().stack",
    );
    assert_eq!(
        stack,
        "Error: boom\n    at inner (t.js:2:10)\n    at outer (t.js:4:27)\n    at t.js:5:1"
    );
}

/// V8 also prints `at Array.map (<anonymous>)` for the native frame; the
/// engine has no frames for natives, so that line is absent.
#[test]
fn constructor_method_and_anonymous_frames_follow_v8() {
    let mut vm = vm();
    let stack = eval_str(
        &mut vm,
        "m.js",
        "class Foo {\n  constructor() { this.s = this.make(); }\n  make() { return [1].map(() => new TypeError('t'))[0].stack; }\n}\nnew Foo().s",
    );
    assert_eq!(
        stack,
        "TypeError: t\n    at m.js:3:33\n    at Foo.make (m.js:3:23)\n    at new Foo (m.js:2:33)\n    at m.js:5:1"
    );
}

#[test]
fn runtime_errors_carry_a_stack_and_columns_count_utf16_units() {
    let mut vm = vm();
    let stack = eval_str(
        &mut vm,
        "u.js",
        "function f() { const s = '\u{1F600}\u{1F600}'; return s.x.y; }\ntry { f(); } catch (e) { e.stack.split('\\n').slice(1).join('|') }",
    );
    assert_eq!(stack, "    at f (u.js:1:45)|    at u.js:2:7");
}

#[test]
fn stack_trace_limit_bounds_the_frames_and_a_non_number_captures_nothing() {
    let mut vm = vm();
    let out = eval_str(
        &mut vm,
        "l.js",
        "function r(n) { return n ? r(n - 1) : new Error('x'); }\nconst a = r(20).stack.split('\\n').length;\nError.stackTraceLimit = 2;\nconst b = r(20).stack.split('\\n').length;\nError.stackTraceLimit = 'x';\nconst c = r(20).stack;\nError.stackTraceLimit = 10;\n[a, b, typeof c].join(' ')",
    );
    assert_eq!(out, "11 3 undefined");
}

#[test]
fn stack_is_an_accessor_on_error_prototype_and_assignable() {
    let mut vm = vm();
    let out = eval_str(
        &mut vm,
        "a.js",
        "const d = Object.getOwnPropertyDescriptor(Error.prototype, 'stack');\nconst e = new Error('m');\nconst own = Object.hasOwn(e, 'stack');\ne.stack = 'custom';\n[typeof d.get, typeof d.set, own, e.stack, Object.getOwnPropertyDescriptor(e, 'stack').enumerable].join()",
    );
    assert_eq!(out, "function,function,false,custom,true");
}

#[test]
fn capture_stack_trace_skips_frames_down_to_the_given_function() {
    let mut vm = vm();
    let out = eval_str(
        &mut vm,
        "c.js",
        "function MyErr() { Error.captureStackTrace(this, MyErr); }\nfunction thrower() { return new MyErr(); }\nconst o = { message: 'hi', name: 'Custom' };\nError.captureStackTrace(o);\nconst t = thrower().stack;\n[t, o.stack, Object.getOwnPropertyDescriptor(o, 'stack').enumerable].join('|')",
    );
    // `MyErr` instances are not errors, so the header is the plain
    // object's: V8 prints `Error` for the missing name and message too.
    assert_eq!(
        out,
        "Error\n    at thrower (c.js:2:29)\n    at c.js:5:11|Custom: hi\n    at c.js:4:7|false"
    );
}

#[test]
fn prepare_stack_trace_receives_call_sites() {
    let mut vm = vm();
    let out = eval_str(
        &mut vm,
        "p.js",
        "Error.prepareStackTrace = (err, sites) => sites.map(s => [s.getFunctionName(), s.getFileName(), s.getLineNumber(), s.getColumnNumber(), s.isToplevel(), s.isConstructor(), String(s)].join(' ')).join('|');\nfunction g() { return new Error('x'); }\nconst s = g().stack;\ndelete Error.prepareStackTrace;\ns",
    );
    assert_eq!(
        out,
        "g p.js 2 23 true false g (p.js:2:23)| p.js 3 11 true false p.js:3:11"
    );
}

#[test]
fn async_frames_continue_across_await() {
    let mut vm = vm();
    let out = run_str(
        &mut vm,
        "as.js",
        "async function inner() {\n  await null;\n  throw new Error('late');\n}\nasync function middle() {\n  await inner();\n}\nasync function outer() {\n  try { await middle(); } catch (e) { globalThis.r = e.stack; }\n}\nouter();",
        "r",
    );
    assert_eq!(
        out,
        "Error: late\n    at inner (as.js:3:9)\n    at async middle (as.js:6:3)\n    at async outer (as.js:9:9)"
    );
}

/// A map for `gen.js` placing it in `src/orig.ts`: generated 1:0 -> 1:0,
/// 1:18 -> 10:3 and 2:0 -> 20:0 (all 0-based).
const MAP: &str = r#"{"version":3,"sources":["orig.ts"],"sourceRoot":"src","names":[],"mappings":"AAAA,kBASG;AAUH"}"#;

#[test]
fn a_host_source_map_rewrites_error_stack_positions() {
    let mut vm = vm();
    vm.set_source_map("gen.js", Some(MAP));
    let out = eval_str(
        &mut vm,
        "gen.js",
        "function boom() { throw new Error('m') }\ntry { boom(); } catch (e) { e.stack }",
    );
    assert_eq!(
        out,
        "Error: m\n    at boom (src/orig.ts:10:4)\n    at src/orig.ts:20:1"
    );
}

#[test]
fn a_malformed_source_map_is_ignored() {
    let mut vm = vm();
    vm.set_source_map("bad.js", Some("{not json"));
    let out = eval_str(&mut vm, "bad.js", "new Error('m').stack");
    assert_eq!(out, "Error: m\n    at bad.js:1:1");
}
