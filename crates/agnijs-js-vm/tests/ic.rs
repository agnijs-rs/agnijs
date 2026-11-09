//! Inline caches must never change what a property access observes: every
//! program below runs the same access site against receivers whose shape,
//! prototype or attributes change under it, and checks the generic answer.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn run(stress: bool, src: &str) -> Value {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "ic-test.js", src)
        .unwrap_or_else(|e| panic!("{e}"));
    vm.run_jobs(realm).unwrap();
    v
}

fn check(src: &str, expected: &str) {
    for stress in [false, true] {
        let v = run(stress, src);
        assert_eq!(
            v,
            Value::string(expected),
            "stress={stress}: {src}"
        );
    }
}

#[test]
fn own_property_reads_follow_shape_changes() {
    check(
        "function f(o) { return o.x; }
         var log = [];
         var a = { x: 1 }, b = { y: 0, x: 2 }, c = { x: 3, z: 1 }, d = {};
         for (var i = 0; i < 6; i++) log.push(f(a), f(b), f(c), f(d));
         a.w = 5; log.push(f(a));
         delete a.x; log.push(f(a));
         log.join()",
        "1,2,3,,1,2,3,,1,2,3,,1,2,3,,1,2,3,,1,2,3,,1,",
    );
}

#[test]
fn prototype_hits_see_later_changes() {
    check(
        "class A { m() { return 'A'; } }
         var a = new A();
         function call(o) { return o.m(); }
         var log = [call(a), call(a), call(a)];
         A.prototype.m = function () { return 'A2'; };
         log.push(call(a));
         a.m = function () { return 'own'; };
         log.push(call(a));
         delete a.m;
         log.push(call(a));
         delete A.prototype.m;
         Object.prototype.m = function () { return 'root'; };
         log.push(call(a));
         Object.setPrototypeOf(a, { m() { return 'swapped'; } });
         log.push(call(a));
         log.join()",
        "A,A,A,A2,own,A2,root,swapped",
    );
}

#[test]
fn accessors_are_not_cached() {
    check(
        "var o = { x: 1 };
         function f() { return o.x; }
         var log = [f(), f()];
         Object.defineProperty(o, 'x', { get() { return 'getter'; }, configurable: true });
         log.push(f());
         Object.defineProperty(o, 'x', { value: 7, writable: true, configurable: true });
         log.push(f());
         var p = { get y() { return 'py'; } };
         var q = Object.create(p);
         function g() { return q.y; }
         log.push(g(), g());
         Object.defineProperty(p, 'y', { value: 'pd', configurable: true });
         log.push(g());
         log.join()",
        "1,1,getter,7,py,py,pd",
    );
}

#[test]
fn cached_writes_respect_later_freezes() {
    check(
        "'use strict';
         var o = { x: 0 };
         function set(v) { o.x = v; }
         set(1); set(2);
         var log = [o.x];
         Object.freeze(o);
         try { set(3); log.push('no throw'); } catch (e) { log.push(e.constructor.name, o.x); }
         var p = { x: 0 };
         function set2(v) { p.x = v; }
         set2(1);
         Object.defineProperty(p, 'x', { set(v) { log.push('setter ' + v); } });
         set2(9);
         log.join()",
        "2,TypeError,2,setter 9",
    );
}

#[test]
fn dictionary_mode_objects_stay_correct() {
    check(
        "var o = {};
         function f() { return o.k50; }
         for (var i = 0; i < 100; i++) o['k' + i] = i;
         var log = [f(), f()];
         o.k50 = 'changed';
         log.push(f());
         delete o.k50;
         log.push(f());
         log.join()",
        "50,50,changed,",
    );
}

#[test]
fn collected_prototypes_do_not_alias() {
    // Prototype objects die and their ids get reused by objects of the same
    // shape; the receiver's prototype identity check must still hold.
    check(
        "function make(v) { var p = { m() { return v; } }; return Object.create(p); }
         function call(o) { return o.m(); }
         var log = [];
         for (var i = 0; i < 200; i++) { var o = make(i); if (i % 50 === 0) log.push(call(o), call(o)); }
         log.join()",
        "0,0,50,50,100,100,150,150",
    );
}
