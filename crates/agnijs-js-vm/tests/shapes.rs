//! Shape transitions share one append-only key table along a chain
//! (TASK-100.27): adding a property is amortised O(1), not a copy of the
//! parent's keys, and lookups through every shape of the chain stay exact.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    // These tests count shapes and key tables, so collections run only
    // where a test asks for one, also under AGNIJS_GC_STRESS.
    vm.heap.stress = false;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> Value {
    let realm = vm.main_realm();
    vm.eval_script(realm, "shapes.js", src).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_linear_chain_of_transitions_shares_one_key_table() {
    let mut vm = vm();
    // The builder runs once with no properties first, so the shapes its
    // own function object and the global `build` binding need exist
    // before the count is taken.
    eval(
        &mut vm,
        "function build(n) {
           var o = {};
           for (var i = 0; i < n; i++) o['p' + i] = i;
           var sum = 0;
           for (var i = 0; i < n; i++) sum += o['p' + i];
           return sum + ',' + Object.keys(o).length + ',' + Object.keys(o)[n - 1];
         }
         build(0);",
    );
    let tables = vm.heap.key_tables.len();
    let shapes = vm.heap.shapes.len();
    // Sixty properties added one at a time (under the dictionary
    // threshold): sixty new shapes, one new key table.
    let v = eval(&mut vm, "build(60)");
    assert_eq!(v, Value::string("1770,60,p59"));
    assert_eq!(vm.heap.shapes.len() - shapes, 60);
    assert_eq!(vm.heap.key_tables.len() - tables, 1);
    // A second object of the same shape reuses the chain: no new shape or
    // table at all.
    eval(&mut vm, "build(60)");
    assert_eq!(vm.heap.shapes.len() - shapes, 60);
    assert_eq!(vm.heap.key_tables.len() - tables, 1);
}

#[test]
fn a_branch_off_a_chain_forks_the_table_and_keeps_both_chains_exact() {
    let mut vm = vm();
    let tables = vm.heap.key_tables.len();
    let v = eval(
        &mut vm,
        "var a = {}; for (var i = 0; i < 20; i++) a['k' + i] = i;
         // Branch off the middle of the chain: same first ten keys, then
         // a different eleventh.
         var b = {}; for (var i = 0; i < 10; i++) b['k' + i] = i * 2;
         b.other = 'x';
         for (var i = 10; i < 15; i++) b['k' + i] = i * 2;
         // Both objects must still read every key, and a key only one of
         // them has must be absent from the other.
         var out = [];
         for (var i = 0; i < 20; i++) out.push(a['k' + i]);
         for (var i = 0; i < 15; i++) out.push(b['k' + i]);
         out.push(b.other, a.other, 'k19' in b, 'k15' in a);
         out.push(Object.keys(a).join('|'), Object.keys(b).join('|'));
         out.join()",
    );
    let mut expect: Vec<String> = (0..20).map(|i| i.to_string()).collect();
    expect.extend((0..15).map(|i| (i * 2).to_string()));
    expect.extend(["x", "", "false", "true"].map(String::from));
    expect.push((0..20).map(|i| format!("k{i}")).collect::<Vec<_>>().join("|"));
    let mut bk: Vec<String> = (0..10).map(|i| format!("k{i}")).collect();
    bk.push("other".into());
    bk.extend((10..15).map(|i| format!("k{i}")));
    expect.push(bk.join("|"));
    assert_eq!(v, Value::string(&expect.join(",")));
    // One table for `a`'s chain, one fork for `b`'s branch.
    assert_eq!(vm.heap.key_tables.len() - tables, 2);
}

#[test]
fn objects_with_a_few_hundred_properties_read_back_every_property() {
    let mut vm = vm();
    let src = "var lit = {".to_owned()
        + &(0..300).map(|i| format!("q{i}: {i}")).collect::<Vec<_>>().join(",")
        + "};
         var inc = {}; for (var i = 0; i < 300; i++) inc['q' + i] = i;
         var ok = true;
         for (var i = 0; i < 300; i++) ok = ok && lit['q' + i] === i && inc['q' + i] === i;
         delete inc.q150; delete lit.q0;
         ok && !('q150' in inc) && !('q0' in lit) && Object.keys(lit).length === 299 && Object.keys(inc)[150] === 'q151'";
    let v = eval(&mut vm, &src);
    assert_eq!(v, Value::Bool(true));
}

// ---- shape lifetime (TASK-100.22) ------------------------------------------

/// Shapes are traced from live objects: once no object, no descendant
/// shape and no live inline cache names a shape, a collection frees it,
/// together with the key table only it used.
#[test]
fn unreachable_shapes_and_their_key_tables_are_freed() {
    let mut vm = vm();
    eval(&mut vm, "function mk(i) { var o = {}; o['u' + i] = i; o['v' + i] = i; return o; } mk(-1);");
    vm.collect_garbage();
    let shapes = vm.heap.live_shapes();
    let tables = vm.heap.live_key_tables();
    // 500 objects, each with its own two-key chain off the empty shape.
    eval(&mut vm, "var keep = []; for (var i = 0; i < 500; i++) keep.push(mk(i));");
    assert!(vm.heap.live_shapes() >= shapes + 1000);
    vm.collect_garbage();
    // (A few unrelated shapes, such as the global object's old ones, may go.)
    assert!(vm.heap.live_shapes() >= shapes + 995, "live objects keep their shapes");
    assert_eq!(
        eval(&mut vm, "keep.every((o, i) => o['u' + i] === i && o['v' + i] === i && Object.keys(o).join() === 'u' + i + ',v' + i)"),
        Value::Bool(true)
    );
    eval(&mut vm, "keep = null;");
    vm.collect_garbage();
    assert!(
        vm.heap.live_shapes() <= shapes + 8,
        "unreachable shapes survived: {} -> {}",
        shapes,
        vm.heap.live_shapes()
    );
    assert!(
        vm.heap.live_key_tables() <= tables + 8,
        "unreachable key tables survived: {} -> {}",
        tables,
        vm.heap.live_key_tables()
    );
    // The atoms only those shapes used died with them.
    assert_eq!(eval(&mut vm, "mk(7).u7 + mk(8).v8"), Value::Number(15.0));
}

/// A live object keeps its whole chain (its shape's ancestors), so new
/// transitions off a surviving shape still find and extend it, while the
/// dead branches next to it are gone.
#[test]
fn a_surviving_shape_keeps_its_ancestors_and_dead_siblings_go() {
    let mut vm = vm();
    let v = eval(
        &mut vm,
        "var live = { a: 1, b: 2, c: 3 };
         for (var i = 0; i < 200; i++) { var t = { a: 1, b: 2 }; t['x' + i] = i; }
         t = null; 'ok'",
    );
    assert_eq!(v, Value::string("ok"));
    vm.collect_garbage();
    let after = vm.heap.live_shapes();
    let v = eval(
        &mut vm,
        "var o = { a: 9, b: 8 }; o.c = 7;
         var p = { a: 1 }; p.b = 2; p.z = 3;
         [o.a, o.b, o.c, Object.keys(o).join(), p.z, Object.keys(p).join(), live.c, Object.keys(live).join()].join(' ')",
    );
    assert_eq!(v, Value::string("9 8 7 a,b,c 3 a,b,z 3 a,b,c"));
    // {a,b,c} was reused rather than rebuilt; only {a,b,z} is new.
    assert!(vm.heap.live_shapes() <= after + 2, "{after} -> {}", vm.heap.live_shapes());
    // Deleting the last key walks back to the (live) parent shape.
    assert_eq!(eval(&mut vm, "delete o.c; o.d = 1; Object.keys(o).join()"), Value::string("a,b,d"));
}

/// Freed shape ids are reused by later shapes. An inline cache in a block
/// no live function or frame holds is not traced, so it may still name a
/// freed id; it must not hit on an object whose new shape got that id.
#[test]
fn a_reused_shape_id_never_hits_a_stale_inline_cache() {
    let mut vm = vm();
    eval(
        &mut vm,
        "function factory() { return function (o) { return o.a; }; }
         var get = factory();
         var seen = 0;
         for (var i = 0; i < 50; i++) { var o = {}; o['p' + i] = 0; o.a = i; seen += get(o); }
         get = null; o = null;",
    );
    // The reader closure and every object it saw are garbage: the shapes
    // its cache names are freed and their ids go on the free list.
    vm.collect_garbage();
    let v = eval(
        &mut vm,
        "var get2 = factory();
         var out = [];
         for (var i = 0; i < 50; i++) {
           // New layouts: `a` first, or absent, over the freed ids.
           var q = {}; q.a = 'A' + i; q['p' + i] = 'P';
           var r = {}; r['p' + i] = 'R'; r['s' + i] = 'S';
           out.push(get2(q), String(get2(r)));
         }
         out.join()",
    );
    let expect: Vec<String> = (0..50).flat_map(|i| [format!("A{i}"), "undefined".to_string()]).collect();
    assert_eq!(v, Value::string(&expect.join(",")));
}

/// A cache in a live function keeps the shapes it names alive, so a hot
/// site does not lose its entries to a collection.
#[test]
fn a_live_inline_cache_keeps_its_shapes() {
    let mut vm = vm();
    eval(
        &mut vm,
        "function get(o) { return o.k; }
         for (var i = 0; i < 4; i++) { var o = {}; o['w' + i] = 1; o.k = i; get(o); }
         o = null;",
    );
    vm.collect_garbage();
    let held = vm.heap.live_shapes();
    let v = eval(
        &mut vm,
        "var s = 0; for (var i = 0; i < 4; i++) { var o = {}; o['w' + i] = 1; o.k = i; s += get(o); } o = null; s",
    );
    assert_eq!(v, Value::Number(6.0));
    assert!(vm.heap.live_shapes() <= held + 1, "the cached shapes were rebuilt: {held} -> {}", vm.heap.live_shapes());
    // Only `get`'s cache held the four receiver shapes and their parents.
    eval(&mut vm, "get = null;");
    vm.collect_garbage();
    assert!(
        held >= vm.heap.live_shapes() + 8,
        "dropping the cache freed fewer than its 8 shapes: {held} -> {}",
        vm.heap.live_shapes()
    );
}
