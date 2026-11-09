//! Calls (TASK-100.19): `Function.prototype.call` and `apply` with a
//! bytecode target, and getters and setters that are bytecode functions,
//! run the target in the current run loop with no native frame. These
//! tests pin that (through `nativeDepth()`, a test native reporting how
//! many native frames are under it) and every behaviour the inlined path
//! must keep: `this` coercion, arguments objects and rest parameters,
//! exceptions reaching the caller's handlers, stack overflow as a
//! catchable RangeError, generators, async functions and class
//! constructors, natives re-entering JavaScript, and a collection at
//! every safe point (each program also runs under GC stress).

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::object::CallArgs;
use agnijs_js_vm::value::{Attrs, Value as VmValue};
use agnijs_js_vm::vm::JsResult;
use agnijs_js_vm::Vm;

/// The native frames under the running code, not counting this native.
fn native_depth(vm: &mut Vm, _a: &CallArgs) -> JsResult<VmValue> {
    Ok(VmValue::Number(vm.native_depth.saturating_sub(1) as f64))
}

fn engine(stress: bool) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    let f = vm.new_native("nativeDepth", 0, native_depth);
    let global = vm.realm().global;
    vm.define_raw_str(global, "nativeDepth", VmValue::Object(f), Attrs::METHOD);
    vm
}

fn run(vm: &mut Vm, src: &str) -> Value {
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "calls-test.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"));
    vm.run_jobs(realm).unwrap();
    v
}

/// Run `src` with and without GC stress; it must evaluate to `expected`.
fn check(src: &str, expected: &str) {
    for stress in [false, true] {
        let mut vm = engine(stress);
        let v = run(&mut vm, src);
        assert_eq!(v, Value::string(expected), "stress={stress}: {src}");
    }
}

#[test]
fn call_and_apply_run_a_bytecode_target_without_a_native_frame() {
    check(
        "function f(a, b) { return nativeDepth() + ':' + a + ',' + b; }
         var log = [f(1, 2), f.call(null, 1, 2), f.apply(null, [1, 2]),
                    f.apply(null), f.apply(null, null), f.call()];
         log.join(' ')",
        "0:1,2 0:1,2 0:1,2 0:undefined,undefined 0:undefined,undefined 0:undefined,undefined",
    );
}

#[test]
fn getters_and_setters_run_without_a_native_frame() {
    check(
        "var seen = [];
         class A { get g() { return nativeDepth(); } set s(v) { seen.push(nativeDepth() + '=' + v); } }
         class B extends A {}
         var o = { get x() { return nativeDepth(); }, set y(v) { seen.push(nativeDepth() + '=' + v); } };
         var b = new B();
         var log = [o.x, b.g];
         var r1 = (o.y = 5), r2 = (b.s = 6);
         log.push(r1, r2, seen.join(','));
         log.join(' ')",
        "0 0 5 6 0=5,0=6",
    );
}

#[test]
fn this_is_coerced_as_for_an_ordinary_call() {
    check(
        "function sloppy() { return typeof this + (this === globalThis ? '!g' : ''); }
         function strict() { 'use strict'; return this === undefined ? 'u' : typeof this; }
         var log = [sloppy.call(null), sloppy.call(undefined), sloppy.call(5), sloppy.apply('s'),
                    strict.call(null), strict.call(5), strict.apply(undefined, []), strict.call('x')];
         log.join(' ')",
        "object!g object!g object object object number u string",
    );
}

#[test]
fn arguments_objects_and_rest_parameters_see_every_argument() {
    check(
        "function f() { return arguments.length + '[' + Array.prototype.join.call(arguments, ',') + ']'; }
         function g(a, ...rest) { return a + '|' + rest.join(','); }
         function m(a) { arguments[0] = 'm'; return a; }
         function h(a, b) { return a + b + arguments.length; }
         var log = [f.call(null, 1, 2, 3), f.apply(null, [4, 5]), f.call(null),
                    g.call(null, 1, 2, 3), g.apply(null, [1]), m.call(null, 'a'),
                    f.apply(null, { length: 2, 0: 'x', 1: 'y' }),
                    f.apply(null, [, 'hole']),
                    (function () { return f.apply(null, arguments); })(7, 8, 9),
                    h.call(null, 1, 2, 3, 4)];
         log.join(' ')",
        "3[1,2,3] 2[4,5] 0[] 1|2,3 1| m 2[x,y] 2[,hole] 3[7,8,9] 7",
    );
}

#[test]
fn apply_rejects_a_non_object_argument_list() {
    check(
        "function f() { return 1; }
         var log = [];
         try { f.apply(null, 1); } catch (e) { log.push(e.constructor.name); }
         try { f.apply(null, 'ab'); } catch (e) { log.push(e.constructor.name); }
         try { Function.prototype.call.call(1); } catch (e) { log.push(e.constructor.name); }
         try { Function.prototype.apply.call({}); } catch (e) { log.push(e.constructor.name); }
         log.join(' ')",
        "TypeError TypeError TypeError TypeError",
    );
}

#[test]
fn exceptions_through_call_apply_and_accessors_reach_the_callers_handler() {
    check(
        "function thrower(x) { throw new Error('t' + x); }
         var o = { get x() { throw new Error('get'); }, set y(v) { throw new Error('set' + v); } };
         var log = [];
         function tryIt(fn) { try { fn(); log.push('no throw'); } catch (e) { log.push(e.message); } finally { log.push('f'); } }
         tryIt(function () { thrower.call(null, 1); });
         tryIt(function () { thrower.apply(null, [2]); });
         tryIt(function () { return o.x; });
         tryIt(function () { o.y = 3; });
         tryIt(function () { [1].forEach(function (v) { thrower.call(null, 'fe'); }); });
         // A handler inside the callee still catches its own throw.
         function inner() { try { throw 'i'; } catch (e) { return 'caught ' + e; } }
         log.push(inner.call(null), inner.apply(null));
         log.join(' ')",
        "t1 f t2 f get f set3 f tfe f caught i caught i",
    );
}

#[test]
fn stack_overflow_through_call_apply_and_getters_is_a_catchable_range_error() {
    // Native recursion (the map callback) is deep in a debug build: run on
    // a stack as large as the test262 runner's.
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(stack_overflow_scenarios)
        .unwrap()
        .join()
        .unwrap();
}

fn stack_overflow_scenarios() {
    for stress in [false, true] {
        let mut vm = engine(stress);
        // A collection at every safe point with 20 000 frames on the stack
        // is quadratic: collect at every 500th instead.
        vm.heap.stress_every = 500;
        let v = run(
            &mut vm,
            "function viaCall() { return viaCall.call(null); }
             function viaApply() { return viaApply.apply(null, []); }
             var o = { get x() { return this.x; } };
             function mixed(n) { return [n].map(function (v) { return mixed.call(null, v + 1); }); }
             var log = [];
             for (var f of [viaCall, viaApply, function () { return o.x; }, function () { return mixed(0); }]) {
               try { f(); log.push('no throw'); } catch (e) { log.push(e instanceof RangeError); }
             }
             log.join(' ')",
        );
        assert_eq!(v, Value::string("true true true true"), "stress={stress}");
        // The VM is usable afterwards: frames, registers and native depth unwound.
        assert_eq!(vm.native_depth, 0);
        assert!(vm.frames.is_empty());
        assert!(vm.regs.is_empty());
        let v = run(&mut vm, "function f(a) { return a * 2; } f.call(null, 21)");
        assert_eq!(v, Value::Number(42.0));
    }
}

#[test]
fn generators_async_functions_and_class_constructors_through_call_and_apply() {
    // The async results arrive after the job queue drains: read them back.
    for stress in [false, true] {
        let mut vm = engine(stress);
        run(
            &mut vm,
            "function* gen(a, b) { yield a; yield b; }
             async function af(x) { await null; return x + 1; }
             class C { constructor() { this.v = 1; } }
             var log = [];
             var it = gen.call(null, 'a', 'b');
             log.push(it.next().value, it.next().value, it.next().done);
             var it2 = gen.apply(null, ['c']);
             log.push(it2.next().value);
             af.call(null, 1).then(function (v) { log.push('af' + v); });
             af.apply(null, [2]).then(function (v) { log.push('af' + v); });
             try { C.call({}); } catch (e) { log.push(e.constructor.name); }
             try { C.apply({}, []); } catch (e) { log.push(e.constructor.name); }",
        );
        let v = run(&mut vm, "log.join(' ')");
        assert_eq!(
            v,
            Value::string("a b true c TypeError TypeError af2 af3"),
            "stress={stress}"
        );
    }
}

#[test]
fn natives_re_entering_javascript_through_call_and_apply() {
    check(
        "function dbl(x) { return x * 2; }
         function cmp(a, b) { return a - b; }
         var log = [];
         log.push([1, 2, 3].map(function (x) { return dbl.call(null, x); }).join(','));
         log.push([3, 1, 2].sort(function (a, b) { return cmp.apply(null, [a, b]); }).join(','));
         log.push(JSON.stringify(JSON.parse('{\"a\":1,\"b\":[2]}', function (k, v) {
           return typeof v === 'number' ? dbl.call(null, v) : v;
         })));
         log.push([1, 2, 3].reduce(function (a, x) { return a + dbl.apply(null, [x]); }, 0));
         // call and apply of natives, bound functions and proxies.
         log.push(Math.max.call(null, 1, 5, 3), Math.max.apply(null, [4, 9]));
         var bound = dbl.bind(null, 10);
         log.push(bound.call(null), bound.apply(null, []));
         var p = new Proxy(dbl, {});
         log.push(p.call(null, 4), p.apply(null, [6]));
         // call through call, and apply of call.
         log.push(Function.prototype.call.call(dbl, null, 7), Function.prototype.apply.call(dbl, null, [8]));
         log.push(dbl.call.call(dbl, null, 9), dbl.call.apply(dbl, [null, 11]));
         log.join(' ')",
        "2,4,6 1,2,3 {\"a\":2,\"b\":[4]} 12 5 9 20 20 8 12 14 16 18 22",
    );
}

#[test]
fn deep_recursion_through_call_keeps_results_and_receivers() {
    check(
        "var o = { n: 0, step(k) { this.n++; return k === 0 ? this.n : this.step.call(this, k - 1); } };
         function sum(k) { return k === 0 ? 0 : k + sum.apply(null, [k - 1]); }
         var log = [o.step.call(o, 500), sum.call(null, 300)];
         log.join(' ')",
        "501 45150",
    );
}

#[test]
fn a_getter_or_setter_inlined_after_a_cache_miss_still_follows_redefinition() {
    check(
        "var proto = { get v() { return 'p' + this.k; } };
         var objs = [];
         for (var i = 0; i < 4; i++) { var o = Object.create(proto); o.k = i; objs.push(o); }
         function read(o) { return o.v; }
         var log = [];
         for (var o of objs) log.push(read(o));
         Object.defineProperty(proto, 'v', { get: function () { return 'q' + this.k; } });
         log.push(read(objs[0]));
         Object.defineProperty(proto, 'v', { value: 'data' });
         log.push(read(objs[1]));
         Object.defineProperty(objs[2], 'v', { get: function () { return 'own'; } });
         log.push(read(objs[2]));
         var m = Object.create(Map.prototype);
         try { m.size; } catch (e) { log.push(e.constructor.name); }
         log.join(' ')",
        "p0 p1 p2 p3 q0 data own TypeError",
    );
}

#[test]
fn assignments_on_the_inlined_setter_path_keep_set_semantics() {
    check(
        "var log = [];
         var ro = { get g() { return 1; } };
         function sloppy(o) { o.g = 2; return o.g; }
         function strict(o) { 'use strict'; o.g = 2; return o.g; }
         log.push(sloppy(ro));
         try { strict(ro); } catch (e) { log.push(e.constructor.name); }
         var frozen = Object.freeze({ x: 1 });
         function setX(o, v) { 'use strict'; o.x = v; return o.x; }
         try { setX(frozen, 2); } catch (e) { log.push('frozen ' + e.constructor.name); }
         var inherited = Object.create(Object.freeze({ y: 1 }));
         function setY(o) { o.y = 5; return o.y; }
         log.push(setY(inherited));
         var seen = [];
         var proxied = Object.create(new Proxy({}, { set(t, k, v, r) { seen.push(k + '=' + v); return true; } }));
         function setZ(o) { o.z = 3; return 'z' in o; }
         log.push(setZ(proxied), seen.join());
         var s = { set w(v) { this._w = v * 2; } };
         function setW(o) { var r = (o.w = 4); return r + ':' + o._w; }
         log.push(setW(s), setW(s));
         var added = {};
         function add(o, i) { o['k' + i] = i; o.fresh = i; return o.fresh; }
         log.push(add(added, 1), Object.keys(added).join());
         log.join(' ')",
        "1 TypeError frozen TypeError 1 false z=3 4:8 4:8 1 k1,fresh",
    );
}
