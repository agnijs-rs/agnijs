//! Allocation weight of objects, functions and string constants
//! (TASK-100.21): string constants are materialised once per block, a
//! function costs at most two Rust heap blocks, small objects keep their
//! slots inline, and none of it is observable from JavaScript.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value as ApiValue};
use agnijs_js_vm::object::{ObjectData, INLINE_SLOTS};
use agnijs_js_vm::value::{ObjId, PropSlot, Value};
use agnijs_js_vm::Vm;

fn vm(stress: bool) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    vm.heap.stress_every = 1;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> ApiValue {
    let realm = vm.main_realm();
    vm.eval_script(realm, "alloc.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"))
}

fn text(vm: &mut Vm, src: &str) -> String {
    match eval(vm, src) {
        ApiValue::String(s) => s.to_string_lossy(),
        other => panic!("expected a string from {src}, got {other:?}"),
    }
}

fn object(vm: &mut Vm, src: &str) -> ObjId {
    let v = eval(vm, src);
    match vm.from_api_value(&v).unwrap() {
        Value::Object(o) => o,
        other => panic!("expected an object from {src}, got {other:?}"),
    }
}

// ---- layout ---------------------------------------------------------------------------------

#[test]
fn slots_are_the_size_of_a_value_and_the_header_has_fixed_offsets() {
    assert_eq!(std::mem::size_of::<PropSlot>(), std::mem::size_of::<Value>());
    assert_eq!(std::mem::size_of::<agnijs_js_vm::object::Class>(), 24);
    assert_eq!(ObjectData::OFFSET_SHAPE, 0);
    // The inline slots are contiguous after the header and the class.
    assert!(ObjectData::OFFSET_CLASS < ObjectData::OFFSET_INLINE);
    assert!(std::mem::size_of::<ObjectData>() <= 160, "{}", std::mem::size_of::<ObjectData>());
}

// ---- string constants -----------------------------------------------------------------------

#[test]
fn a_string_constant_is_not_allocated_again_after_a_collection() {
    // A collection at every safe point: before TASK-100.21 each load after
    // a collection made a new string.
    let mut vm = vm(true);
    eval(
        &mut vm,
        "function long() { return 'a string constant longer than thirty-two code units'; }
         function short() { return 'short'; }
         long(); short();",
    );
    let before = vm.heap.strings_allocated;
    eval(
        &mut vm,
        "var n = 0; for (var i = 0; i < 2000; i++) n += long().length + short().length; n",
    );
    let made = vm.heap.strings_allocated - before;
    assert!(made < 50, "{made} strings for 4000 constant loads");
}

#[test]
fn string_constants_keep_their_value_across_collections() {
    let mut vm = vm(true);
    let s = text(
        &mut vm,
        "function f(i) { return i % 2 ? 'odd constant, longer than thirty-two units' : 'even'; }
         var out = '';
         for (var i = 0; i < 6; i++) { var x = f(i); out += x.length + ':' + x.slice(0, 4) + ','; }
         out",
    );
    assert_eq!(s, "4:even,42:odd ,4:even,42:odd ,4:even,42:odd ,");
}

// ---- functions ------------------------------------------------------------------------------

#[test]
fn creating_a_function_allocates_no_name_string() {
    let mut vm = vm(false);
    eval(&mut vm, "function make() { return function named(a, b) { return a; }; } make();");
    let before = vm.heap.strings_allocated;
    eval(&mut vm, "for (var i = 0; i < 1000; i++) make();");
    let made = vm.heap.strings_allocated - before;
    assert!(made < 20, "{made} strings for 1000 functions");
}

#[test]
fn a_function_owns_at_most_two_heap_blocks() {
    let mut vm = vm(false);
    for src in [
        "(function () { return function f(a) { return a; }; })()",
        "(function () { 'use strict'; return function f(a) { return a; }; })()",
        "(() => (x) => x + 1)()",
        "({ m() {} }).m",
        "(function* g() {})",
        "(async function a() {})",
        "(class C { constructor() {} })",
    ] {
        let f = object(&mut vm, src);
        let blocks = vm.heap.obj(f).owned_blocks();
        assert!(blocks <= 2, "{src}: {blocks} blocks");
        // A prototype object holds `constructor` inline.
        let proto = text(&mut vm, &format!("String(Object.getOwnPropertyNames({src}))"));
        if proto.contains("prototype") {
            let p = object(&mut vm, &format!("({src}).prototype"));
            assert_eq!(vm.heap.obj(p).owned_blocks(), 0, "{src}: prototype object");
        }
    }
}

#[test]
fn function_properties_are_unchanged() {
    let mut vm = vm(false);
    let describe = "function describe(f) {
        return Object.getOwnPropertyNames(f).map(function (k) {
            var d = Object.getOwnPropertyDescriptor(f, k);
            var v = typeof d.value === 'object' && d.value !== null ? 'obj' : String(d.value);
            return k + '=' + v + ':' + (d.writable ? 'w' : '') + (d.enumerable ? 'e' : '') + (d.configurable ? 'c' : '');
        }).join(' ');
    }";
    eval(&mut vm, describe);
    let cases = [
        ("function sloppy(a, b) {}", "length=2:c name=sloppy:c caller=undefined:c arguments=undefined:c prototype=obj:w"),
        ("(function strict(a) { 'use strict'; })", "length=1:c name=strict:c prototype=obj:w"),
        ("((a, b, c) => a)", "length=3:c name=:c"),
        ("({ m(x) {} }).m", "length=1:c name=m:c"),
        ("(function* gen() {})", "length=0:c name=gen:c prototype=obj:w"),
        ("(async function af() {})", "length=0:c name=af:c"),
        ("(async function* ag() {})", "length=0:c name=ag:c prototype=obj:w"),
        ("(class K { constructor(a) {} static s() {} })", "length=1:c name=K:c prototype=obj: s=s() {}:wc"),
        ("(function () { var anon = function () {}; return anon; })()", "length=0:c name=anon:c caller=undefined:c arguments=undefined:c prototype=obj:w"),
    ];
    for (src, want) in cases {
        let got = text(&mut vm, &format!("describe({src})"));
        assert_eq!(got, want, "{src}");
    }
    // The prototype object: `constructor` only, not enumerable.
    assert_eq!(
        text(&mut vm, "function P() {} describe(P.prototype) + ' ' + (P.prototype.constructor === P)"),
        "constructor=function P() {}:wc true"
    );
    // Redefining and deleting still work on the eager properties.
    assert_eq!(
        text(&mut vm, "function Q() {} delete Q.name; Object.defineProperty(Q, 'length', { value: 7 }); Q.prototype = 1; describe(Q)"),
        "length=7:c caller=undefined:c arguments=undefined:c prototype=1:w"
    );
}

#[test]
fn functions_survive_gc_stress() {
    let mut vm = vm(true);
    let s = text(
        &mut vm,
        "function mk(i) { var c = i; return function (d) { c += d; return c; }; }
         var fs = [];
         for (var i = 0; i < 50; i++) fs.push(mk(i), (x) => x + i);
         var s = 0;
         for (var j = 0; j < fs.length; j++) s += fs[j](1);
         s + ',' + fs[0].name + ',' + fs[1].name.length + ',' + Object.keys(fs[0].prototype).length",
    );
    assert_eq!(s, format!("{},,0,0", (0..50).map(|i| i + 1 + 50 + 1).sum::<i32>()));
}

// ---- adding properties by a known transition ------------------------------------------------

#[test]
fn a_cached_property_add_respects_later_changes_to_the_prototype_chain() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        let s = text(
            &mut vm,
            "'use strict';
             function P() {} var log = [];
             function add(o) { o.v = 1; return o; }
             for (var i = 0; i < 5; i++) add(new P());
             // A setter appears on the prototype: the add must call it.
             Object.defineProperty(P.prototype, 'v', { set(x) { log.push('set' + x); }, configurable: true });
             var a = add(new P());
             log.push(Object.keys(a).length);
             delete P.prototype.v;
             for (var i = 0; i < 5; i++) add(new P());
             // A read-only inherited property: the strict add throws.
             Object.defineProperty(Object.prototype, 'v', { value: 0, writable: false, configurable: true });
             try { add(new P()); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
             delete Object.prototype.v;
             for (var i = 0; i < 5; i++) add(new P());
             // A non-extensible receiver of the same shape.
             try { add(Object.preventExtensions(new P())); log.push('no throw'); } catch (e) { log.push(e.constructor.name); }
             // A receiver that is itself a prototype: objects inheriting
             // from it see the new property.
             var q = new P(); var child = Object.create(q);
             function rd(o) { return o.v; }
             for (var i = 0; i < 5; i++) rd(child);
             add(q); log.push(rd(child));
             // A different prototype with the key.
             var r = Object.create({ set v(x) { log.push('other' + x); } });
             add(r); log.push(Object.keys(r).length);
             log.join(',')",
        );
        assert_eq!(s, "set1,0,TypeError,TypeError,1,other1,0", "stress {stress}");
    }
}

#[test]
fn object_literals_with_repeated_and_computed_keys() {
    let mut vm = vm(false);
    let s = text(
        &mut vm,
        "var out = [];
         for (var i = 0; i < 4; i++) {
           var o = { a: i, b: 2, a: 3, ['c' + i]: 4 };
           out.push(JSON.stringify(o));
         }
         var frozenProto = Object.freeze({ a: 9 });
         var o = { __proto__: frozenProto, a: 1 };
         out.push(o.a, Object.getOwnPropertyDescriptor(o, 'a').writable);
         out.join(' ')",
    );
    assert_eq!(
        s,
        r#"{"a":3,"b":2,"c0":4} {"a":3,"b":2,"c1":4} {"a":3,"b":2,"c2":4} {"a":3,"b":2,"c3":4} 1 true"#
    );
}

// ---- inline slots ---------------------------------------------------------------------------

#[test]
fn small_objects_keep_their_slots_inline() {
    let mut vm = vm(false);
    let o = object(&mut vm, "({ a: 1, b: 2, c: 3 })");
    assert_eq!(vm.heap.obj(o).owned_blocks(), 0);
    let o = object(&mut vm, "var o = {}; o.a = 1; o.b = 2; o.c = 3; o.d = 4; o");
    assert_eq!(vm.heap.obj(o).slot_count(), INLINE_SLOTS);
    assert_eq!(vm.heap.obj(o).owned_blocks(), 0);
    let o = object(&mut vm, "({ a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 })");
    assert_eq!(vm.heap.obj(o).owned_blocks(), 1, "the overflow buffer");
}

#[test]
fn properties_across_the_inline_boundary() {
    for stress in [false, true] {
        let mut vm = vm(stress);
        let s = text(
            &mut vm,
            "var o = {};
             for (var i = 0; i < 9; i++) o['k' + i] = i * 10;
             var a = JSON.stringify(o);
             delete o.k8; delete o.k7; delete o.k6; delete o.k5; delete o.k4;
             var b = JSON.stringify(o);
             o.k4 = 'x'; o.k5 = 'y';
             var c = JSON.stringify(o);
             delete o.k1;
             o.z = 1;
             var d = JSON.stringify(o);
             var g = { get v() { return this.k0 + 1; }, set v(x) { this.k0 = x; }, p: 1, q: 2, r: 3 };
             g.v = 41;
             a + '|' + b + '|' + c + '|' + d + '|' + g.v + ',' + g.k0 + ',' + Object.keys(g)",
        );
        assert_eq!(
            s,
            "{\"k0\":0,\"k1\":10,\"k2\":20,\"k3\":30,\"k4\":40,\"k5\":50,\"k6\":60,\"k7\":70,\"k8\":80}\
             |{\"k0\":0,\"k1\":10,\"k2\":20,\"k3\":30}\
             |{\"k0\":0,\"k1\":10,\"k2\":20,\"k3\":30,\"k4\":\"x\",\"k5\":\"y\"}\
             |{\"k0\":0,\"k2\":20,\"k3\":30,\"k4\":\"x\",\"k5\":\"y\",\"z\":1}\
             |42,41,v,p,q,r,k0",
            "stress {stress}"
        );
    }
}

#[test]
fn a_removed_inline_slot_does_not_keep_its_value_alive() {
    let mut vm = vm(false);
    eval(
        &mut vm,
        "var o = { a: 1, b: {} }; var w = new WeakRef(o.b); delete o.b;
         var big = { a: 1, b: 2, c: 3, d: 4, e: {} }; var w2 = new WeakRef(big.e); delete big.e;",
    );
    let realm = vm.main_realm();
    vm.run_jobs(realm).unwrap();
    vm.collect_garbage();
    assert_eq!(
        text(&mut vm, "String(w.deref()) + ',' + String(w2.deref())"),
        "undefined,undefined"
    );
}
