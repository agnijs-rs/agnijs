//! Precompiled code through the engine interface: `compile_script` and
//! `compile_module` produce serialised register bytecode in the shared
//! container, a fresh engine runs it without the source being recompiled,
//! and anything else is refused before it runs.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{bytecode, Engine, Error, ModuleSource, PromiseState, RealmId, Value};
use agnijs_js_vm::engine::FORMAT;
use std::rc::Rc;

fn engine() -> (Rc<TestHost>, Box<dyn Engine>, RealmId) {
    let host = TestHost::new();
    let e = agnijs_js_vm::make_engine(host.clone());
    let realm = e.main_realm();
    (host, e, realm)
}

fn run_bytecode(bytes: &[u8]) -> (Box<dyn Engine>, RealmId, Value) {
    let (_host, mut e, realm) = engine();
    let v = e
        .eval_bytecode(realm, bytes)
        .unwrap_or_else(|err| panic!("eval_bytecode: {err}"));
    e.run_jobs(realm).unwrap();
    (e, realm, v)
}

fn eval(e: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    e.eval_script(realm, "probe.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

#[test]
fn the_engine_names_its_bytecode_format_and_uses_the_shared_container() {
    let (_host, mut e, realm) = engine();
    // Pins the tag on purpose: bumping it must be a visible change (ADR 0010).
    assert_eq!(FORMAT, "agnijs-bc-8");
    assert_eq!(e.info().bytecode_format, FORMAT);
    let bytes = e.compile_script(realm, "a.js", "1 + 1").unwrap();
    assert!(bytecode::unwrap(FORMAT, &bytes).is_ok());
}

const PROGRAM: &str = r#"
const log = [];
function counter() { let n = 0; return () => ++n; }
const c = counter(); c(); log.push(c());
class A { #x = 1; static k = 'K'; get x() { return this.#x; } }
class B extends A { constructor() { super(); this.y = super.x + 1; } }
log.push(new B().x, new B().y, B.k);
function* g() { const v = yield 1; yield v * 2; }
const it = g(); it.next(); log.push(it.next(21).value);
const tag = (s) => s;
const t1 = () => tag`a${1}b`;
log.push(t1() === t1(), t1().raw.join('|'));
log.push(/(?<d>\d+)/u.exec('ab12').groups.d, 2n ** 64n + '');
async function af() { await null; log.push('async'); }
af();
try { null.x; } catch (e) { log.push(e.constructor.name); }
globalThis.log = log;
log.join(',');
"#;

#[test]
fn precompiled_scripts_run_in_a_fresh_engine() {
    let (_host, mut compiler, realm) = engine();
    let bytes = compiler
        .compile_script(realm, "app/main.js", PROGRAM)
        .unwrap();
    let (mut e, realm, v) = run_bytecode(&bytes);
    assert_eq!(
        v,
        Value::string("2,1,2,K,42,true,a|b,12,18446744073709551616,TypeError")
    );
    // The async continuation ran when the host drained jobs.
    assert_eq!(
        eval(&mut *e, realm, "log[log.length - 1]"),
        Value::string("async")
    );
}

#[test]
fn source_positions_and_text_survive_precompilation() {
    let src = "function outer() {\n  return function inner(a, b) { return a + b; };\n}\n\
               globalThis.inner = outer();\n\
               function boom() {\n  throw new RangeError('deep');\n}\n";
    let (_host, mut compiler, realm) = engine();
    let bytes = compiler.compile_script(realm, "app/pos.js", src).unwrap();
    let (mut e, realm, _) = run_bytecode(&bytes);
    assert_eq!(
        eval(&mut *e, realm, "inner.toString()"),
        Value::string("function inner(a, b) { return a + b; }")
    );
    assert_eq!(e.script_source(realm, "app/pos.js").as_deref(), Some(src));
    let err = e.eval_script(realm, "call.js", "boom()").unwrap_err();
    let exc = err.exception().expect("exception");
    assert_eq!(exc.name, "RangeError");
    assert_eq!(exc.stack[0].script, "app/pos.js");
    assert_eq!(exc.stack[0].line, 6);
    assert_eq!(exc.stack[0].function, "boom");
}

#[test]
fn precompiled_modules_link_against_source_and_bytecode_modules() {
    let (_host, mut compiler, realm) = engine();
    let main = compiler
        .compile_module(
            realm,
            "app/main.js",
            "import { dep } from './dep.js';\nimport { lib } from './lib.js';\n\
             export const total = dep + lib + await Promise.resolve(1);\n\
             export const meta = import.meta.url;",
        )
        .unwrap();
    let lib = compiler
        .compile_module(realm, "app/lib.js", "export const lib = 20;")
        .unwrap();

    let host = TestHost::new();
    host.modules
        .borrow_mut()
        .insert("app/main.js".into(), ModuleSource::Bytecode(main));
    host.modules
        .borrow_mut()
        .insert("app/lib.js".into(), ModuleSource::Bytecode(lib));
    host.add_module("app/dep.js", "export const dep = 21;");
    let mut e = agnijs_js_vm::make_engine(host.clone());
    let realm = e.main_realm();
    let p = e.load_module(realm, "app/main.js").unwrap();
    e.run_jobs(realm).unwrap();
    let ns = match e.promise_state(realm, &p).unwrap() {
        PromiseState::Fulfilled(ns) => ns,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        e.get(realm, &ns, &"total".into()).unwrap(),
        Value::Number(42.0)
    );
    assert_eq!(
        e.get(realm, &ns, &"meta".into()).unwrap(),
        Value::string("test://app/main.js")
    );
}

#[test]
fn another_engines_bytecode_is_a_version_mismatch() {
    let (_host, mut e, realm) = engine();
    let foreign = bytecode::wrap("quickjs-ng-0.16", b"\x02\x00");
    match e.eval_bytecode(realm, &foreign) {
        Err(Error::BytecodeMismatch { expected, found }) => {
            assert_eq!(expected, FORMAT);
            assert_eq!(found, "quickjs-ng-0.16");
        }
        other => panic!("{other:?}"),
    }
    let garbage = b"definitely not bytecode";
    assert!(matches!(
        e.eval_bytecode(realm, garbage),
        Err(Error::BytecodeMismatch { .. })
    ));
    assert!(matches!(
        e.eval_bytecode(realm, &[]),
        Err(Error::BytecodeMismatch { .. })
    ));
}

#[test]
fn a_future_format_of_this_engine_is_a_version_mismatch() {
    let (_host, mut e, realm) = engine();
    let bytes = e.compile_script(realm, "a.js", "1").unwrap();
    let payload = bytecode::unwrap(FORMAT, &bytes).unwrap().to_vec();
    let newer = bytecode::wrap("agnijs-bc-9", &payload);
    assert!(matches!(
        e.eval_bytecode(realm, &newer),
        Err(Error::BytecodeMismatch { found, .. }) if found == "agnijs-bc-9"
    ));
}

/// A container whose debug section (statement starts, locals) is malformed
/// but whose checksum is right: the section is decoded only when a debugger
/// needs it, so the code runs, and a debugger that is attached when the
/// bytes are loaded verifies the section then and refuses the container.
fn container_with_a_bad_debug_section() -> Vec<u8> {
    use agnijs_js_compiler::{compile_script, DebugSection};
    let code = compile_script(
        "function f(x) { let y = x + 1; return y; }\nglobalThis.ran = f(1);",
        "bad.js",
    )
    .unwrap();
    let mut owned = Rc::try_unwrap(code).expect("sole owner");
    let mut debug = std::mem::take(&mut owned.debug).into_info().unwrap();
    debug.statements.push(agnijs_js_compiler::StatementStart {
        pc: owned.instrs.len() as u32,
        offset: 0,
    });
    owned.debug = DebugSection::from(debug);
    agnijs_js_vm::engine::encode_bytecode(false, &owned)
}

#[test]
fn a_malformed_debug_section_does_not_stop_the_code_from_running() {
    let (_host, mut e, realm) = engine();
    e.eval_bytecode(realm, &container_with_a_bad_debug_section())
        .unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(eval(&mut *e, realm, "ran"), Value::Number(2.0));
    let err = e.eval_script(realm, "t.js", "f(null); f.x.y").unwrap_err();
    let exc = err.exception().expect("exception");
    assert_eq!(exc.name, "TypeError");
    assert_eq!((exc.stack[0].script.as_str(), exc.stack[0].line), ("t.js", 1));
}

#[test]
fn an_attached_debugger_refuses_a_malformed_debug_section_at_load() {
    use agnijs_js_debugger::*;
    struct Quiet;
    impl DebugClient for Quiet {
        fn paused(&self, _: &mut dyn Debuggee, _: &PauseInfo) -> ResumeAction {
            ResumeAction::Continue
        }
    }
    let mut d = agnijs_js_vm::make_debuggee(TestHost::new());
    d.attach(Rc::new(Quiet));
    let realm = d.engine().main_realm();
    match d
        .engine()
        .eval_bytecode(realm, &container_with_a_bad_debug_section())
    {
        Err(Error::BytecodeCorrupt(m)) => assert!(m.contains("statement"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        eval(d.engine(), realm, "typeof ran"),
        Value::string("undefined"),
        "nothing ran from a refused container"
    );
}

/// The M2 source-payload container (ADR 0010): a 4-byte magic, `agnijs-bc-1`,
/// kind, then the name and source text with u32 lengths. Its loader was
/// removed (no build ever wrote it under the `agnijs-bc-1` tag), so it is
/// refused like any other foreign container.
#[test]
fn source_payload_containers_from_before_m7_are_refused() {
    let (_host, mut e, realm) = engine();
    let mut old = b"GBLCagnijs-bc-1\x01".to_vec(); // magic, tag, script kind
    for s in ["old.js", "globalThis.ran = true"] {
        old.extend_from_slice(&(s.len() as u32).to_le_bytes());
        old.extend_from_slice(s.as_bytes());
    }
    assert!(matches!(
        e.eval_bytecode(realm, &old),
        Err(Error::BytecodeMismatch { .. })
    ));
    assert_eq!(eval(&mut *e, realm, "typeof ran"), Value::string("undefined"));
}

#[test]
fn a_corrupt_payload_is_refused_without_running() {
    let (_host, mut e, realm) = engine();
    let bytes = e
        .compile_script(
            realm,
            "c.js",
            "globalThis.ran = true; [1, 2].map(x => x * 2)",
        )
        .unwrap();
    let header = bytes.len() - bytecode::unwrap(FORMAT, &bytes).unwrap().len();
    // Truncated.
    for cut in [header, header + 1, bytes.len() / 2, bytes.len() - 1] {
        assert!(
            matches!(
                e.eval_bytecode(realm, &bytes[..cut]),
                Err(Error::BytecodeCorrupt(_))
            ),
            "cut at {cut}"
        );
    }
    // Every single-bit and single-byte change in the payload is caught by
    // the payload checksum before anything is decoded or run.
    for i in header..bytes.len() {
        for mask in [0x01u8, 0x5A, 0x80] {
            let mut bad = bytes.clone();
            bad[i] ^= mask;
            let (_h, mut fresh, r) = engine();
            match fresh.eval_bytecode(r, &bad) {
                Err(Error::BytecodeCorrupt(_)) => {}
                other => panic!("byte {i} ^ {mask:#x}: {other:?}"),
            }
        }
    }
    assert_eq!(
        eval(&mut *e, realm, "typeof ran"),
        Value::string("undefined"),
        "nothing ran from a refused container"
    );
}
