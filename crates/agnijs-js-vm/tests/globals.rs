//! The global-binding cache must never change what a global read or write
//! observes: every program below runs the same access site many times, so
//! the site is cached, then changes the binding under it and checks the
//! generic answer.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

/// Run `scripts` one after another in one realm; the last one's value.
fn run(stress: bool, scripts: &[&str]) -> Value {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    let realm = vm.main_realm();
    let mut v = Value::Undefined;
    for (i, src) in scripts.iter().enumerate() {
        v = vm
            .eval_script(realm, &format!("globals-{i}.js"), src)
            .unwrap_or_else(|e| panic!("{e}"));
        vm.run_jobs(realm).unwrap();
    }
    v
}

fn check(scripts: &[&str], expected: &str) {
    for stress in [false, true] {
        let v = run(stress, scripts);
        assert_eq!(v, Value::string(expected), "stress={stress}: {scripts:?}");
    }
}

#[test]
fn a_cached_property_read_follows_writes_deletes_and_redefinitions() {
    check(
        &["globalThis.p = 1;
           function f() { return p; }
           for (var i = 0; i < 50; i++) f();
           var log = [f()];
           globalThis.p = 2; log.push(f());
           p = 3; log.push(f(), globalThis.p);
           delete globalThis.p;
           try { f(); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
           log.push(typeof p);
           globalThis.p = 4; log.push(f());
           var n = 0;
           Object.defineProperty(globalThis, 'p', { get() { return 'acc' + (++n); }, configurable: true });
           log.push(f(), f());
           Object.defineProperty(globalThis, 'p', { value: 9, writable: false, configurable: true });
           log.push(f());
           p = 10; log.push(f());
           log.join()"],
        "1,2,3,3,ReferenceError,undefined,4,acc1,acc2,9,9",
    );
}

#[test]
fn a_cached_property_write_respects_later_attribute_changes_and_deletes() {
    check(
        &["globalThis.q = 0;
           function set(v) { q = v; }
           function setStrict(v) { 'use strict'; q = v; }
           for (var i = 0; i < 50; i++) { set(i); setStrict(i); }
           var log = [q];
           Object.defineProperty(globalThis, 'q', { value: 'ro', writable: false, configurable: true });
           set(1); log.push(q);
           try { setStrict(2); log.push('no throw'); } catch (e) { log.push(e.constructor.name, q); }
           var calls = 0;
           Object.defineProperty(globalThis, 'q', { set(v) { calls += v; }, configurable: true });
           set(5); setStrict(7); log.push(calls);
           delete globalThis.q;
           try { setStrict(3); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
           log.push('q' in globalThis);
           set(8); log.push(q, Object.getOwnPropertyDescriptor(globalThis, 'q').enumerable);
           log.join()"],
        "49,ro,TypeError,ro,12,ReferenceError,false,8,true",
    );
}

#[test]
fn a_later_global_lexical_shadows_a_cached_global_property() {
    check(
        &[
            "globalThis.s = 'prop'; globalThis.t = 'prop';
             function rs() { return s; }
             function rt() { return t; }
             function ws(v) { s = v; }
             for (var i = 0; i < 50; i++) { rs(); rt(); ws('prop'); }
             rs()",
            "var tdz;
             try { rt(); tdz = 'no throw'; } catch (e) { tdz = e.constructor.name; }
             let s = 'lex';
             const t = 'clex';
             ws('lex2');
             var c;
             try { (function () { t = 1; })(); c = 'no throw'; } catch (e) { c = e.constructor.name; }
             [rs(), rt(), s, globalThis.s, tdz, c].join()",
        ],
        "lex2,clex,lex2,prop,ReferenceError,TypeError",
    );
}

#[test]
fn cached_lexical_reads_and_writes_see_each_other() {
    check(
        &["let a = 0;
           const k = 'k';
           function get() { return a; }
           function set(v) { a = v; }
           function setK() { k = 'x'; }
           var log = [];
           for (var i = 0; i < 50; i++) { set(i); if (get() !== i) log.push('miss' + i); }
           log.push(get());
           a = 'direct'; log.push(get());
           set('viaSet'); log.push(a);
           try { setK(); log.push('no throw'); } catch (e) { log.push(e.constructor.name, k); }
           try { setK(); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
           log.join()"],
        "49,direct,viaSet,TypeError,k,TypeError",
    );
}

#[test]
fn global_reads_of_builtins_and_this_keep_working() {
    check(
        &["function f() { return Math.max(1, 2) + Object.keys({ a: 1 }).length + Array.isArray([]); }
           var r = 0;
           for (var i = 0; i < 50; i++) r = f();
           var g = (function () { return this; })();
           [r, g === globalThis, typeof undefinedName, typeof globalThis].join()"],
        "4,true,undefined,object",
    );
}

/// The ticket's reproduction: a million bare-name reads of one global.
/// Run in release mode with `--ignored --nocapture` to read the time.
#[test]
#[ignore]
fn reproduction_global_reads() {
    // The same loop over a local gives the floor: the loop's own cost.
    let global = "globalThis.GG = 1;
                  function f() { var s = 0; for (var i = 0; i < 1000000; i++) s += GG; return s; }
                  f(); f()";
    let local = "function f() { var s = 0, LL = 1; for (var i = 0; i < 1000000; i++) s += LL; return s; }
                 f(); f()";
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    for (what, src) in [("global", global), ("local", local)] {
        let mut best = f64::MAX;
        for _ in 0..7 {
            let t = std::time::Instant::now();
            let v = vm.eval_script(realm, "repro.js", src).unwrap();
            assert_eq!(v, Value::Number(1_000_000.0));
            best = best.min(t.elapsed().as_secs_f64() * 1000.0 / 2.0);
        }
        eprintln!("1 M {what} reads: {best:.1} ms (best of 7)");
    }
}
