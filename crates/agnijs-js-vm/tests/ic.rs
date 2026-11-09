//! Inline caches must never change what a property access observes: every
//! program below runs the same access site against receivers whose shape,
//! prototype or attributes change under it, and checks the generic answer.
//! Each runs with and without GC stress (a collection at every safe point,
//! so cached ids, prototypes and string constants die and are reused).

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
        assert_eq!(v, Value::string(expected), "stress={stress}: {src}");
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
fn accessors_follow_redefinition() {
    check(
        "var o = { x: 1 };
         function f() { return o.x; }
         var log = [f(), f()];
         Object.defineProperty(o, 'x', { get() { return 'getter'; }, configurable: true });
         log.push(f(), f());
         Object.defineProperty(o, 'x', { value: 7, writable: true, configurable: true });
         log.push(f());
         var p = { get y() { return 'py'; } };
         var q = Object.create(p);
         function g() { return q.y; }
         log.push(g(), g());
         Object.defineProperty(p, 'y', { value: 'pd', configurable: true });
         log.push(g());
         log.join()",
        "1,1,getter,getter,7,py,py,pd",
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

// ---- polymorphic and megamorphic sites ------------------------------------------------

#[test]
fn polymorphic_sites_keep_four_shapes_apart() {
    check(
        "function f(o) { return o.a; }
         var objs = [{ a: 1, b: 1 }, { b: 1, a: 2 }, { a: 3, b: 1, c: 2 }, { c: 2, b: 1, a: 4 }];
         var log = [];
         for (var r = 0; r < 3; r++) for (var i = 0; i < 4; i++) log.push(f(objs[i]));
         objs[1].a = 20; objs[3].a = 40;
         for (var i = 0; i < 4; i++) log.push(f(objs[i]));
         delete objs[2].c;
         for (var i = 0; i < 4; i++) log.push(f(objs[i]));
         log.join()",
        "1,2,3,4,1,2,3,4,1,2,3,4,1,20,3,40,1,20,3,40",
    );
}

#[test]
fn megamorphic_sites_stay_correct() {
    check(
        "function f(o) { return o.a; }
         var objs = [];
         for (var i = 0; i < 12; i++) { var o = {}; o['k' + i] = 0; o.a = i; objs.push(o); }
         var log = [];
         for (var r = 0; r < 2; r++) for (var i = 0; i < 12; i++) log.push(f(objs[i]));
         objs[5].a = 'x';
         log.push(f(objs[5]), f({ a: 'new' }), f({}));
         log.join()",
        "0,1,2,3,4,5,6,7,8,9,10,11,0,1,2,3,4,5,6,7,8,9,10,11,x,new,",
    );
}

#[test]
fn polymorphic_methods_on_two_classes() {
    // The method-calls shape: two classes whose instances share a receiver
    // shape and differ in their prototype; a getter two levels up.
    check(
        "class S { constructor(n) { this.n = n; } area() { return this.n; } get double() { return this.n * 2; } }
         class Q extends S { area() { return super.area() * this.n; } }
         var xs = [new S(2), new Q(3), new S(4), new Q(5)];
         function run() { var out = []; for (var i = 0; i < xs.length; i++) out.push(xs[i].area(), xs[i].double); return out.join(); }
         var log = [run(), run()];
         Q.prototype.area = function () { return 'q'; };
         log.push(run());
         Object.defineProperty(S.prototype, 'double', { get() { return 'd' + this.n; }, configurable: true });
         log.push(run());
         Object.defineProperty(Q.prototype, 'double', { value: 'shadow', configurable: true });
         log.push(run());
         log.join(' | ')",
        "2,4,9,6,4,8,25,10 | 2,4,9,6,4,8,25,10 | 2,4,q,6,4,8,q,10 | 2,d2,q,d3,4,d4,q,d5 | 2,d2,q,shadow,4,d4,q,shadow",
    );
}

// ---- deeper prototype chains -------------------------------------------------------

#[test]
fn depth_two_holders_see_changes_in_between() {
    check(
        "var top = { m: 'top' };
         var mid = Object.create(top);
         var o = Object.create(mid);
         function f() { return o.m; }
         var log = [f(), f()];
         mid.m = 'mid'; log.push(f());
         delete mid.m; log.push(f());
         top.m = 'top2'; log.push(f());
         Object.defineProperty(top, 'm', { value: 'ro', writable: false, configurable: true }); log.push(f());
         Object.setPrototypeOf(mid, { m: 'other' }); log.push(f());
         Object.setPrototypeOf(mid, top); log.push(f());
         delete top.m; log.push(f());
         Object.prototype.m = 'root'; log.push(f());
         delete Object.prototype.m;
         log.join()",
        "top,top,mid,top,top2,ro,other,ro,,root",
    );
}

#[test]
fn proxies_on_the_chain_are_never_cached() {
    check(
        "var count = 0;
         var top = { m: 'top' };
         var mid = Object.create(top);
         var o = Object.create(mid);
         function f() { return o.m; }
         var log = [f(), f()];
         Object.setPrototypeOf(mid, new Proxy(top, { get(t, k, r) { count++; return 'trap'; } }));
         log.push(f(), f(), count);
         var p = new Proxy({ m: 'target' }, {});
         function g(x) { return x.m; }
         log.push(g(p), g(p));
         log.join()",
        "top,top,trap,trap,2,target,target",
    );
}

#[test]
fn getters_and_setters_through_the_cache() {
    check(
        "'use strict';
         var log = [];
         var base = { get v() { return 'get:' + this.id; }, set v(x) { log.push('set:' + this.id + '=' + x); } };
         var mid = Object.create(base);
         function mk(id) { var o = Object.create(mid); o.id = id; return o; }
         var a = mk('a'), b = mk('b');
         function rd(o) { return o.v; }
         function wr(o, x) { o.v = x; }
         for (var i = 0; i < 3; i++) { log.push(rd(a), rd(b)); wr(a, i); }
         Object.defineProperty(base, 'v', { get: undefined, set: undefined, configurable: true });
         log.push(String(rd(a)));
         try { wr(a, 9); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
         var own = { get w() { return 'own' + this.k; }, set w(x) { this.k = x; }, k: 1 };
         function rw(o) { return o.w; }
         function ww(o, x) { o.w = x; }
         log.push(rw(own)); ww(own, 2); log.push(rw(own)); ww(own, 3); log.push(rw(own));
         var native = Object.create(Object.getPrototypeOf(new Map()));
         function sz(m) { try { return m.size; } catch (e) { return e.constructor.name; } }
         var m = new Map([[1, 1]]);
         log.push(sz(m), sz(m), sz(native));
         log.join()",
        "get:a,get:b,set:a=0,get:a,get:b,set:a=1,get:a,get:b,set:a=2,undefined,TypeError,own1,own2,own3,1,1,TypeError",
    );
}

#[test]
fn getters_that_throw_or_reenter() {
    check(
        "var o = { n: 0, get g() { this.n++; if (this.n === 3) throw new Error('boom'); return this.n; } };
         function f() { try { return o.g; } catch (e) { return e.message; } }
         var r = { get self() { return this.depth > 0 ? (this.depth--, this.self) : 'done'; }, depth: 3 };
         function s() { return r.self; }
         [f(), f(), f(), f(), s()].join()",
        "1,2,boom,4,done",
    );
}

// ---- arrays and strings --------------------------------------------------------------

#[test]
fn array_length_and_methods() {
    check(
        "var a = [1, 2, 3];
         function len(x) { return x.length; }
         var log = [len(a), len(a)];
         a.push(4); log.push(len(a));
         a.length = 1; log.push(len(a));
         a[9] = 0; log.push(len(a));
         var plain = { length: 'plain' };
         log.push(len(plain), len(a), len('str'), len(plain));
         var fake = Object.create([5, 6, 7]);
         log.push(len(fake));
         var arr2 = [1]; Object.setPrototypeOf(arr2, plain);
         var obj2 = Object.create(plain);
         log.push(len(obj2), len(arr2), len(obj2));
         function find(x, v) { return x.indexOf(v); }
         var b = [1, 2];
         find(b, 1); find(b, 2);
         Array.prototype.indexOf = function (v) { return 'patched'; };
         log.push(find(b, 3), b.length);
         log.join()",
        "3,3,4,1,10,plain,10,3,plain,3,plain,1,plain,patched,2",
    );
}

#[test]
fn string_length_and_methods() {
    check(
        "var strs = ['abc', 'de', '', 'fghij'];
         function len(s) { return s.length; }
         function code(s) { return s.charCodeAt(0); }
         var log = [];
         for (var i = 0; i < 4; i++) log.push(len(strs[i]), code(strs[i]));
         String.prototype.charCodeAt = function () { return 'patched'; };
         log.push(code('x'));
         delete String.prototype.charCodeAt;
         Object.prototype.charCodeAt = function () { return 'root'; };
         log.push(code('x'));
         var so = new String('wxyz');
         log.push(len(so), len(so));
         log.join()",
        "3,97,2,100,0,NaN,5,102,patched,root,4,4",
    );
}

#[test]
fn string_methods_per_realm() {
    for stress in [false, true] {
        let mut vm = Vm::new(TestHost::new());
        vm.heap.stress = stress;
        let main = vm.main_realm();
        let other = Engine::create_realm(&mut vm).unwrap();
        Engine::eval_script(
            &mut vm,
            other,
            "other.js",
            "String.prototype.which = function () { return 'other'; };
             function otherWhich(s) { return s.which(); }",
        )
        .unwrap();
        let og = Engine::global_object(&mut vm, other).unwrap();
        let f = Engine::get(&mut vm, other, &og, &"otherWhich".into()).unwrap();
        let mg = Engine::global_object(&mut vm, main).unwrap();
        Engine::set(&mut vm, main, &mg, &"otherWhich".into(), &f).unwrap();
        let v = vm
            .eval_script(
                main,
                "main.js",
                "String.prototype.which = function () { return 'main'; };
                 function which(s) { return s.which(); }
                 var log = [];
                 for (var i = 0; i < 3; i++) log.push(which('a'), otherWhich('a'));
                 log.join()",
            )
            .unwrap();
        assert_eq!(
            v,
            Value::string("main,other,main,other,main,other"),
            "stress={stress}"
        );
    }
}

// ---- keyed access ------------------------------------------------------------------

#[test]
fn keyed_reads_of_dense_and_odd_elements() {
    check(
        "var a = [10, , 30];
         Array.prototype[1] = 'proto';
         function at(x, i) { return x[i]; }
         var log = [at(a, 0), at(a, 1), at(a, 2), at(a, 3), at(a, -0), at(a, 1.5), at(a, NaN), at(a, -1)];
         delete Array.prototype[1];
         log.push(at(a, 1));
         log.push(at(new String('xyz'), 1), at('xyz', 2), at(new Uint8Array([7, 8]), 1));
         (function () { log.push(at(arguments, 0)); arguments[0] = 'mapped'; })('arg');
         function m(p) { arguments[0] = 'changed'; return at(arguments, 0) + p; }
         log.push(m('p'));
         var sparse = []; sparse[100000] = 's'; log.push(at(sparse, 100000));
         var o = { 0: 'o0' }; log.push(at(o, 0));
         var g = []; Object.defineProperty(g, 0, { get() { return 'getter'; } }); log.push(at(g, 0));
         log.push(at(a, 4294967295), at(a, 4294967294));
         log.join()",
        "10,proto,30,,10,,,,,y,z,8,arg,changedchanged,s,o0,getter,,",
    );
}

#[test]
fn keyed_writes_respect_holes_and_attributes() {
    check(
        "'use strict';
         function put(x, i, v) { x[i] = v; }
         var log = [];
         var a = [1, 2, 3];
         put(a, 0, 'a'); put(a, 2, 'c'); put(a, 3, 'd'); put(a, 5, 'f');
         log.push(a.join('/'), a.length);
         var setterHit = '';
         Object.defineProperty(Array.prototype, 4, { set(v) { setterHit = v; }, configurable: true });
         put(a, 4, 'e');
         delete Array.prototype[4];
         log.push(setterHit, 4 in a);
         var f = Object.freeze([1, 2]);
         try { put(f, 0, 'x'); log.push('no throw'); } catch (e) { log.push(e.constructor.name, f[0]); }
         var s = Object.seal([1, 2]); put(s, 1, 'sealed'); log.push(s[1]);
         var ro = [1, 2]; Object.defineProperty(ro, 1, { writable: false });
         try { put(ro, 1, 'x'); log.push('no throw'); } catch (e) { log.push(e.constructor.name, ro[1]); }
         var ta = new Int8Array(2); put(ta, 1, 300); log.push(ta[1]);
         log.join()",
        "a/2/c/d//f,6,e,false,TypeError,1,sealed,TypeError,2,44",
    );
}

// ---- string constants --------------------------------------------------------------

#[test]
fn string_constants_survive_collections() {
    check(
        "class A { m() { return 'A'; } }
         class B extends A { m() { return super.m() + 'B'; } }
         var b = new B();
         var out = [];
         var long = '';
         for (var i = 0; i < 50; i++) {
           var k = 'key';
           var t = 'a string constant longer than thirty-two code units';
           out.push(b.m(), k, t.length, ({ key: i })['key']);
           long += 'x';
         }
         [out.slice(0, 4).join(), out.slice(-4).join(), long.length].join(' ')",
        "AB,key,51,0 AB,key,51,49 50",
    );
}
