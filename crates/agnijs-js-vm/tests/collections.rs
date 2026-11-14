//! Map and Set reclaim the slots deleted entries leave (TASK-103.32), and
//! live iterators, `forEach` and the Set methods keep their place in
//! insertion order across deletions, `clear` and that compaction.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, Value};
use agnijs_js_vm::Vm;

const MIB: usize = 1 << 20;

fn vm() -> Vm {
    let mut vm = Vm::new(TestHost::new());
    // The churn loops would collect at every step under AGNIJS_GC_STRESS.
    vm.heap.stress = false;
    vm
}

fn eval(vm: &mut Vm, src: &str) -> Result<Value, Error> {
    let realm = vm.main_realm();
    vm.eval_script(realm, "t.js", src)
}

fn eval_str(src: &str) -> Value {
    let mut vm = vm();
    eval(&mut vm, src).unwrap_or_else(|e| panic!("{src}: {e:?}"))
}

// ---- memory -----------------------------------------------------------------------------------

/// The probe from the task: `m.set(k, v); m.delete(k)` in a loop. Every
/// slot a collection holds is counted, so before compaction the count grew
/// by a slot per round.
#[test]
fn set_delete_churn_keeps_memory_usage_bounded() {
    for (name, src) in [
        (
            "Map",
            "globalThis.c = new Map(); for (let i = 0; i < 1e6; i++) { c.set(i, i); c.delete(i); }",
        ),
        (
            "Set",
            "globalThis.c = new Set(); for (let i = 0; i < 1e6; i++) { c.add(i); c.delete(i); }",
        ),
        (
            "Map with live entries",
            "globalThis.c = new Map(); for (let i = 0; i < 1000; i++) c.set(-i - 1, i);
             for (let i = 0; i < 1e6; i++) { c.set(i, i); c.delete(i); }",
        ),
    ] {
        let mut vm = vm();
        eval(&mut vm, "globalThis.c = null").unwrap();
        vm.collect_garbage();
        let base = vm.memory_usage().heap_bytes;
        eval(&mut vm, src).unwrap();
        vm.collect_garbage();
        let after = vm.memory_usage().heap_bytes;
        assert!(after < base + MIB, "{name}: {base} -> {after}");
    }
}

/// The same churn runs to the end under a 16 MiB limit instead of
/// reaching it.
#[test]
fn set_delete_churn_runs_under_a_memory_limit() {
    let mut vm = vm();
    vm.collect_garbage();
    let used = vm.memory_usage().heap_bytes;
    vm.set_memory_limit(Some(used + 16 * MIB));
    let v = eval(
        &mut vm,
        "const m = new Map(), s = new Set();
         for (let i = 0; i < 2e6; i++) { m.set(i, i); m.delete(i); s.add(i); s.delete(i); }
         m.size + s.size",
    )
    .unwrap();
    assert_eq!(v, Value::Number(0.0));
}

// ---- live iterators ---------------------------------------------------------------------------

/// An iterator paused in the middle sees the entries after its position
/// that are still there, in order, however many deletions compacted the
/// collection meanwhile.
#[test]
fn a_paused_iterator_survives_compaction() {
    let got = eval_str(
        "const m = new Map(); for (let i = 0; i < 1000; i++) m.set(i, i);
         const it = m.keys();
         const out = [it.next().value, it.next().value];
         // Delete most entries before and after the position.
         for (let i = 0; i < 1000; i++) if (i % 100 !== 50) m.delete(i);
         for (const k of it) out.push(k);
         out.join()",
    );
    assert_eq!(
        got,
        Value::string("0,1,50,150,250,350,450,550,650,750,850,950")
    );
}

#[test]
fn a_paused_set_iterator_survives_compaction_and_sees_new_entries() {
    let got = eval_str(
        "const s = new Set(); for (let i = 0; i < 100; i++) s.add(i);
         const it = s.values();
         const out = [it.next().value];
         for (let i = 0; i < 100; i++) if (i !== 99) s.delete(i);
         s.add('a'); s.add(0); s.add('b');
         for (const k of it) out.push(k);
         out.join()",
    );
    assert_eq!(got, Value::string("0,99,a,0,b"));
}

/// An entry deleted and added again moves to the end, and an iterator
/// that passed it sees it again there.
#[test]
fn a_re_added_entry_is_visited_again_at_the_end() {
    let got = eval_str(
        "const m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]);
         const out = [];
         let n = 0;
         for (const [k, v] of m) {
           out.push(k + v);
           if (k === 1 && v === 'a') { m.delete(1); for (let i = 10; i < 200; i++) { m.set(i, i); m.delete(i); } m.set(1, 'z'); }
           if (++n > 10) break;
         }
         out.join()",
    );
    assert_eq!(got, Value::string("1a,2b,3c,1z"));
}

/// `clear` empties the collection; an iterator taken before it goes on
/// with what is added afterwards.
#[test]
fn an_iterator_continues_after_clear() {
    let got = eval_str(
        "const s = new Set([1, 2, 3]);
         const it = s.values();
         const out = [it.next().value];
         s.clear();
         s.add(4); s.add(1);
         for (const k of it) out.push(k);
         out.push(s.size);
         out.join()",
    );
    assert_eq!(got, Value::string("1,4,1,2"));
}

/// An iterator that finished stays finished, even when entries are added.
#[test]
fn a_finished_iterator_stays_done() {
    let got = eval_str(
        "const m = new Map([[1, 1]]);
         const it = m.keys();
         it.next(); const a = it.next().done;
         m.set(2, 2);
         String(a) + it.next().done",
    );
    assert_eq!(got, Value::string("truetrue"));
}

/// Several iterators at different positions of one collection.
#[test]
fn several_iterators_keep_their_own_positions() {
    let got = eval_str(
        "const m = new Map(); for (let i = 0; i < 300; i++) m.set(i, i);
         const a = m.keys(), b = m.keys();
         for (let i = 0; i < 10; i++) a.next();
         for (let i = 0; i < 200; i++) b.next();
         for (let i = 0; i < 300; i++) if (i % 50) m.delete(i);
         [...a].join() + '|' + [...b].join()",
    );
    assert_eq!(got, Value::string("50,100,150,200,250|200,250"));
}

// ---- forEach and the Set methods --------------------------------------------------------------

#[test]
fn for_each_deleting_ahead_and_behind_keeps_its_place() {
    let got = eval_str(
        "const m = new Map(); for (let i = 0; i < 500; i++) m.set(i, i);
         const out = [];
         m.forEach((v, k) => {
           out.push(k);
           if (k === 3) for (let i = 0; i < 500; i++) if (i !== 3 && i % 100 !== 7) m.delete(i);
           if (k === 307) m.set('end', 1);
         });
         out.join()",
    );
    assert_eq!(got, Value::string("0,1,2,3,7,107,207,307,407,end"));
}

#[test]
fn set_for_each_survives_delete_all_and_re_add() {
    let got = eval_str(
        "const s = new Set(); for (let i = 0; i < 100; i++) s.add(i);
         const out = [];
         s.forEach(k => {
           out.push(k);
           if (k === 0) { for (let i = 0; i < 100; i++) s.delete(i); s.add('x'); }
         });
         out.join()",
    );
    assert_eq!(got, Value::string("0,x"));
}

/// `isSubsetOf` walks the receiver while the argument's `has` deletes
/// from it.
#[test]
fn set_methods_walk_the_receiver_across_compaction() {
    let got = eval_str(
        "const s = new Set(); for (let i = 0; i < 200; i++) s.add(i);
         const seen = [];
         const other = {
           size: 1000,
           has(k) { seen.push(k); if (k === 1) for (let i = 2; i < 200; i++) if (i % 50) s.delete(i); return true; },
           keys() { return [].values(); },
         };
         const r = s.isSubsetOf(other);
         seen.push(r);
         const seen2 = [];
         const t = new Set(); for (let i = 0; i < 100; i++) t.add(i);
         const inter = t.intersection({
           size: 1000,
           has(k) { seen2.push(k); if (k === 0) for (let i = 1; i < 100; i++) if (i !== 77) t.delete(i); return true; },
           keys() { return [].values(); },
         });
         seen.join() + '|' + seen2.join() + '|' + [...inter].join()",
    );
    assert_eq!(got, Value::string("0,1,50,100,150,true|0,77|0,77"));
}
