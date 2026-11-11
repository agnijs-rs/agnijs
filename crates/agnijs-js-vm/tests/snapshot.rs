//! Realm snapshots (TASK-101.2): an initialised realm, built-ins plus an
//! embedder-run bootstrap script, written with `Engine::snapshot` and
//! restored with `make_engine_from_snapshot`, through the engine interface
//! only.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, RealmId, Value};

fn engine() -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine(TestHost::new());
    let realm = e.main_realm();
    (e, realm)
}

fn eval(e: &mut dyn Engine, realm: RealmId, src: &str) -> Value {
    e.eval_script(realm, "t.js", src)
        .unwrap_or_else(|err| panic!("{err}: {src}"))
}

fn snapshot_of(bootstrap: &str) -> Vec<u8> {
    let (mut e, realm) = engine();
    eval(&mut *e, realm, bootstrap);
    e.snapshot(realm).unwrap_or_else(|err| panic!("snapshot: {err}"))
}

fn restore(bytes: &[u8]) -> (Box<dyn Engine>, RealmId) {
    let e = agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), bytes)
        .unwrap_or_else(|err| panic!("restore: {err}"));
    let realm = e.main_realm();
    (e, realm)
}

#[test]
fn a_bootstrapped_realm_survives_a_round_trip() {
    let bytes = snapshot_of(
        "var counter = 40;
         function bump(n) { counter += n; return counter; }
         globalThis.config = { name: 'app', sizes: [1, 2, 3] };",
    );
    let (mut e, realm) = restore(&bytes);
    assert_eq!(eval(&mut *e, realm, "bump(2)"), Value::Number(42.0));
    assert_eq!(
        eval(&mut *e, realm, "config.name + config.sizes.length"),
        Value::string("app3")
    );
    assert_eq!(eval(&mut *e, realm, "[1, 2].map(x => x * 2).join()"), Value::string("2,4"));
}

#[test]
fn closures_classes_and_builtin_state_survive() {
    let bytes = snapshot_of(
        "const makeCounter = () => { let n = 0; return () => ++n; };
         const tick = makeCounter(); tick(); tick();
         class Point {
           #x; static origin = new Point(0, 0);
           constructor(x, y) { this.#x = x; this.y = y; }
           get x() { return this.#x; }
           #secret() { return 'p' + this.#x; }
           reveal() { return this.#secret(); }
         }
         class Point3 extends Point { z = 7; }
         const registry = new Map([['a', 1], [{}, 2]]);
         const seen = new Set([1, 'two', 3n]);
         const weak = new WeakMap([[Point, 'cls']]);
         const bytes = new Uint8Array([1, 2, 3, 250]);
         const view = new DataView(bytes.buffer, 1, 2);
         const re = /(\\d+)-(\\w+)/g; re.lastIndex = 0;
         const tag = s => s;
         const tpl = () => tag`a${1}b`;
         const firstTpl = tpl();
         const sym = Symbol.for('app.key');
         const own = Symbol('own');
         globalThis[own] = 'mine';
         var big = 2n ** 70n;
         const date = new Date(0);
         const frozen = Object.freeze({ k: 1 });
         const sparse = []; sparse[100] = 'far';
         const bound = function (a, b) { return this.v + a + b; }.bind({ v: 1 }, 2);
         const settled = Promise.resolve(5);
         const err = new TypeError('boom');",
    );
    let (mut e, realm) = restore(&bytes);
    let checks: &[(&str, Value)] = &[
        ("tick()", Value::Number(3.0)),
        ("new Point(4, 5).x + new Point(4, 5).y", Value::Number(9.0)),
        ("new Point(6, 0).reveal()", Value::string("p6")),
        ("Point.origin.x", Value::Number(0.0)),
        ("new Point3(1, 2).z", Value::Number(7.0)),
        ("new Point3(1, 2) instanceof Point", Value::Bool(true)),
        ("registry.get('a') + registry.size", Value::Number(3.0)),
        ("seen.has(3n) && seen.has('two') && !seen.has(2)", Value::Bool(true)),
        ("weak.get(Point)", Value::string("cls")),
        ("bytes[3] + view.getUint8(0)", Value::Number(252.0)),
        ("re.exec('12-ab')[2]", Value::string("ab")),
        ("tpl() === firstTpl", Value::Bool(true)),
        ("sym === Symbol.for('app.key') && Symbol.keyFor(sym) === 'app.key'", Value::Bool(true)),
        ("globalThis[own]", Value::string("mine")),
        ("String(big)", Value::string("1180591620717411303424")),
        ("date.getTime()", Value::Number(0.0)),
        ("Object.isFrozen(frozen) && frozen.k === 1", Value::Bool(true)),
        ("sparse.length + sparse[100]", Value::string("101far")),
        ("bound(3)", Value::Number(6.0)),
        ("err instanceof TypeError && err.message", Value::string("boom")),
        ("Symbol.iterator in Array.prototype", Value::Bool(true)),
    ];
    for (src, want) in checks {
        assert_eq!(&eval(&mut *e, realm, src), want, "{src}");
    }
    eval(&mut *e, realm, "var out; settled.then(v => { out = v; });");
    e.run_jobs(realm).unwrap();
    assert_eq!(eval(&mut *e, realm, "out"), Value::Number(5.0));
}

#[test]
fn a_restored_realm_keeps_script_scoping_rules() {
    let bytes = snapshot_of("let fixed = 1; var loose = 2;");
    let (mut e, realm) = restore(&bytes);
    let err = e.eval_script(realm, "u.js", "let fixed = 3;").unwrap_err();
    assert!(err.is_exception_named("SyntaxError"), "{err}");
    let err = e.eval_script(realm, "u.js", "let loose = 3;").unwrap_err();
    assert!(err.is_exception_named("SyntaxError"), "{err}");
    assert_eq!(eval(&mut *e, realm, "fixed + loose"), Value::Number(3.0));
}

#[test]
fn a_restored_engine_collects_garbage_and_snapshots_again() {
    let bytes = snapshot_of("var keep = { list: [1, 2, 3] };");
    let (mut e, realm) = restore(&bytes);
    eval(&mut *e, realm, "for (let i = 0; i < 1000; i++) ({ i }); keep.more = 4;");
    e.collect_garbage();
    assert_eq!(eval(&mut *e, realm, "keep.list.length + keep.more"), Value::Number(7.0));
    let again = e.snapshot(realm).unwrap();
    let (mut e2, realm2) = restore(&again);
    assert_eq!(eval(&mut *e2, realm2, "keep.more"), Value::Number(4.0));
}

#[test]
fn snapshots_are_deterministic() {
    let src = "var a = { x: 1, y: [2, 3] }; const m = new Map([[1, 2]]); function f() { return a; }";
    assert_eq!(snapshot_of(src), snapshot_of(src));
}

fn refusal(setup: impl FnOnce(&mut dyn Engine, RealmId)) -> String {
    let (mut e, realm) = engine();
    setup(&mut *e, realm);
    match e.snapshot(realm) {
        Err(Error::SnapshotUnsupported(m)) => m,
        Err(other) => panic!("expected a refusal, got {other}"),
        Ok(_) => panic!("the snapshot was taken"),
    }
}

#[test]
fn state_a_snapshot_cannot_carry_is_refused_with_its_path() {
    let m = refusal(|e, realm| {
        let f = e
            .new_function(realm, "hostFn", std::rc::Rc::new(|_e: &mut dyn Engine, _cx| Ok(Value::Undefined)))
            .unwrap();
        let o = eval(e, realm, "globalThis.app = { hooks: {} }; app.hooks");
        e.set(realm, &o, &agnijs_engine_api::PropertyKey::from("onLoad"), &f).unwrap();
    });
    assert!(m.contains("host function") && m.contains("globalThis.app.hooks.onLoad"), "{m}");

    let m = refusal(|e, realm| {
        eval(e, realm, "globalThis.gen = (function* () { yield 1; })();");
    });
    assert!(m.contains("generator") && m.contains("globalThis.gen"), "{m}");

    let m = refusal(|e, realm| {
        eval(e, realm, "var resolveLater; new Promise(r => { resolveLater = r; });");
    });
    assert!(m.contains("captured state"), "{m}");

    let m = refusal(|e, realm| {
        eval(e, realm, "Promise.resolve(1).then(() => {});");
    });
    assert!(m.contains("jobs"), "{m}");
}

fn expect_corrupt(bytes: &[u8]) {
    match agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), bytes) {
        Err(Error::BytecodeCorrupt(_)) | Err(Error::BytecodeMismatch { .. }) => {}
        Err(other) => panic!("expected a corrupt-input error, got {other}"),
        Ok(_) => panic!("a damaged snapshot was accepted"),
    }
}

#[test]
fn damaged_and_truncated_snapshots_are_rejected() {
    let bytes = snapshot_of("var x = { a: [1, 2], f() { return 1; } };");
    expect_corrupt(&[]);
    expect_corrupt(b"AGJS");
    for cut in [5, 20, bytes.len() / 2, bytes.len() - 1] {
        expect_corrupt(&bytes[..cut]);
    }
    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x40;
    expect_corrupt(&flipped);
    let mut longer = bytes.clone();
    longer.push(0);
    expect_corrupt(&longer);
}

/// A deterministic xorshift generator, so a failing case reproduces.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Mutations past the checksum (resealed) never make the loader panic.
/// When a mutation leaves the code trees alone and the snapshot still
/// loads, running script over the damaged heap must not panic either:
/// it may throw. Mutated code is the bytecode loader's trust model (ADR
/// 0010) and is only loaded, not run.
#[test]
fn fuzzed_snapshots_load_or_fail_without_panicking() {
    use agnijs_js_vm::snapshot::testing::{code_span, reseal};
    let base = snapshot_of(
        "let n = 0; const inc = () => ++n;
         class A { #p = 1; static s = [1, { t: 'x' }]; get p() { return this.#p; } }
         var m = new Map([['k', new Set([1])]]), ta = new Int16Array(4), re = /a+/i;
         var b = inc.bind(null); var sym = Symbol.for('s'); var dv = new DataView(new ArrayBuffer(8));",
    );
    let span = code_span(&base).expect("code span");
    let start = 4 + agnijs_js_vm::snapshot::format_tag().len() + 1 + 16;
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let probe = "try { inc(); new A().p; A.s[1].t; m.get('k').has(1); ta[1] = 3; re.test('AA'); b(); \
                 Symbol.keyFor(sym); dv.setInt8(0, 1); Object.keys(globalThis).length; } catch (e) {}";
    let (mut loaded, mut ran) = (0, 0);
    let iters: usize = std::env::var("SNAPSHOT_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    let seed: u64 = std::env::var("SNAPSHOT_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9e37_79b9_7f4a_7c15);
    rng.0 = seed;
    for _ in 0..iters {
        let mut bytes = base.clone();
        let edits = 1 + rng.below(4);
        let mut touches_code = false;
        for _ in 0..edits {
            let at = start + rng.below(bytes.len() - start);
            touches_code |= span.contains(&at);
            bytes[at] = match rng.below(4) {
                0 => bytes[at] ^ (1 << rng.below(8)),
                1 => 0,
                2 => 0xff,
                _ => rng.next() as u8,
            };
        }
        reseal(&mut bytes);
        let Ok(mut e) = agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), &bytes) else {
            continue;
        };
        loaded += 1;
        if touches_code {
            continue;
        }
        let realm = e.main_realm();
        e.set_fuel(Some(200_000)).unwrap();
        let _ = e.eval_script(realm, "probe.js", probe);
        e.collect_garbage();
        ran += 1;
    }
    eprintln!("fuzz: {loaded} of {iters} loaded, {ran} ran the probe");
}

#[test]
fn a_snapshot_from_another_format_version_is_rejected() {
    let mut bytes = snapshot_of("var x = 1;");
    // The container's format tag follows the four-byte magic.
    let at = bytes.iter().position(|&b| b == b'-').unwrap();
    bytes[at] = b'_';
    match agnijs_js_vm::make_engine_from_snapshot(TestHost::new(), &bytes) {
        Err(Error::BytecodeMismatch { .. }) => {}
        Err(other) => panic!("expected a version mismatch, got {other}"),
        Ok(_) => panic!("a mismatched snapshot was accepted"),
    }
}
