//! Shape transitions share one append-only key table along a chain
//! (TASK-100.27): adding a property is amortised O(1), not a copy of the
//! parent's keys, and lookups through every shape of the chain stay exact.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value};
use agnijs_js_vm::Vm;

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
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
