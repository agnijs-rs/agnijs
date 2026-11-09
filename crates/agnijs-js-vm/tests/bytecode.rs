//! Precompiled code through the engine interface: `compile_script` and
//! `compile_module` produce serialised register bytecode in the shared
//! container, a fresh engine runs it without the source being recompiled,
//! and anything else is refused before it runs.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{bytecode, Engine, Error, ModuleSource, PromiseState, RealmId, Value};
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
    assert_eq!(e.info().bytecode_format, "agnijs-bc-3");
    let bytes = e.compile_script(realm, "a.js", "1 + 1").unwrap();
    assert!(bytecode::unwrap("agnijs-bc-3", &bytes).is_ok());
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
            assert_eq!(expected, "agnijs-bc-3");
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
    let payload = bytecode::unwrap("agnijs-bc-3", &bytes).unwrap().to_vec();
    let newer = bytecode::wrap("agnijs-bc-4", &payload);
    assert!(matches!(
        e.eval_bytecode(realm, &newer),
        Err(Error::BytecodeMismatch { found, .. }) if found == "agnijs-bc-4"
    ));
}

/// The M2 container (ADR 0010): `GBLC`, `agnijs-bc-1`, kind, then the name
/// and source text with u32 lengths.
fn legacy_container(module: bool, name: &str, source: &str) -> Vec<u8> {
    let mut out = b"GBLCagnijs-bc-1".to_vec();
    out.push(if module { 2 } else { 1 });
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(source.len() as u32).to_le_bytes());
    out.extend_from_slice(source.as_bytes());
    out
}

#[test]
fn source_payload_containers_from_before_m7_still_load() {
    let (_host, mut e, realm) = engine();
    let v = e
        .eval_bytecode(
            realm,
            &legacy_container(false, "old.js", "'legacy:' + (40 + 2)"),
        )
        .unwrap();
    assert_eq!(v, Value::string("legacy:42"));
    let mut other_tag = legacy_container(false, "old.js", "1");
    other_tag[13] = b'9'; // agnijs-bc-9
    assert!(matches!(
        e.eval_bytecode(realm, &other_tag),
        Err(Error::BytecodeMismatch { .. })
    ));
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
    let header = bytes.len() - bytecode::unwrap("agnijs-bc-3", &bytes).unwrap().len();
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
