//! Promise jobs and `await` (TASK-100.25): resolving functions and await
//! reactions that script cannot observe are internal records, not function
//! objects, while job order, the resolving functions script can see and the
//! async stacks stay as they were. test262 is the oracle for the order of
//! jobs; the cases here pin down the interleavings that changed paths touch.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Value as ApiValue};
use agnijs_js_vm::Vm;

fn vm(stress: bool) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    vm.heap.stress_every = 1;
    vm
}

/// Run `src` as a script, drain the job queue, and return `log.join(' ')`.
fn order(vm: &mut Vm, src: &str) -> String {
    let realm = vm.main_realm();
    vm.eval_script(realm, "order.js", &format!("var log = [];\n{src}"))
        .unwrap_or_else(|e| panic!("{e}: {src}"));
    vm.run_jobs(realm).unwrap();
    match vm.eval_script(realm, "log.js", "log.join(' ')").unwrap() {
        ApiValue::String(s) => s.to_string_lossy(),
        other => panic!("log is {other:?}"),
    }
}

fn both(src: &str, want: &str) {
    for stress in [false, true] {
        assert_eq!(order(&mut vm(stress), src), want, "stress {stress}: {src}");
    }
}

// ---- job order ------------------------------------------------------------------------------

#[test]
fn await_and_then_interleave_as_specified() {
    both(
        "async function a1() { log.push('a1'); await a2(); log.push('a1 end'); }
         async function a2() { log.push('a2'); }
         log.push('start');
         a1();
         new Promise(function (r) { log.push('p1'); r(); }).then(function () { log.push('p2'); });
         log.push('end');",
        "start a1 a2 p1 end a1 end p2",
    );
    // Awaiting a native promise takes one tick, as does a primitive.
    both(
        "(async function () { await Promise.resolve(); log.push('A'); await 1; log.push('A2'); })();
         Promise.resolve().then(function () { log.push('B'); }).then(function () { log.push('C'); });",
        "A B A2 C",
    );
}

#[test]
fn an_async_function_returning_a_promise_takes_two_extra_ticks() {
    both(
        "async function f() { return Promise.resolve(1); }
         f().then(function (v) { log.push('f' + v); });
         Promise.resolve().then(function () { log.push(1); }).then(function () { log.push(2); })
           .then(function () { log.push(3); }).then(function () { log.push(4); });",
        "1 2 f1 3 4",
    );
}

#[test]
fn awaiting_a_thenable_calls_then_in_a_job() {
    both(
        "var t = { then: function (res, rej) { log.push('then:' + typeof res + res.length + typeof rej); res(7); res(8); rej(9); } };
         (async function () { try { log.push('got' + await t); } catch (e) { log.push('caught' + e); } })();
         log.push('sync');
         Promise.resolve().then(function () { log.push('tick'); });",
        "sync then:function1function tick got7",
    );
}

#[test]
fn rejections_resume_the_body_with_a_throw() {
    both(
        "async function f() {
           try { await Promise.reject(new Error('x')); } catch (e) { log.push('c1:' + e.message); }
           try { await { get then() { throw 'g'; } }; } catch (e) { log.push('c2:' + e); }
           var p = Promise.reject(3); p.catch(function () {});
           try { await p; } catch (e) { log.push('c3:' + e); }
           throw 'out';
         }
         f().catch(function (e) { log.push('rejected:' + e); });",
        "c1:x c2:g c3:3 rejected:out",
    );
}

#[test]
fn then_chains_resolve_in_registration_order() {
    both(
        "var p = Promise.resolve(0);
         p.then(function () { log.push('a'); });
         p.then(function () { log.push('b'); return Promise.resolve(); }).then(function () { log.push('b2'); });
         p.then(function () { log.push('c'); throw 1; }).catch(function (e) { log.push('c' + e); });
         var q = new Promise(function (r) { setTimeoutish = r; });
         q.then(function (v) { log.push('q' + v); });
         q.then(undefined, function () { log.push('never'); }).then(function (v) { log.push('pass' + v); });
         p.then(function () { setTimeoutish(5); });
         p.finally(function () { log.push('fin'); }).then(function (v) { log.push('after' + v); });",
        "a b c fin c1 q5 pass5 b2 after0",
    );
}

#[test]
fn async_generators_settle_requests_in_order() {
    both(
        "async function* g() { log.push('in'); yield 1; await null; yield 2; return 3; }
         var it = g();
         function show(tag) { return function (r) { log.push(tag + ':' + r.value + ':' + r.done); }; }
         it.next().then(show('n1'));
         it.next().then(show('n2'));
         it.next().then(show('n3'));
         it.return(Promise.resolve(9)).then(show('r'));
         it.next().then(show('n4'));
         Promise.resolve().then(function () { log.push('t1'); }).then(function () { log.push('t2'); })
           .then(function () { log.push('t3'); }).then(function () { log.push('t4'); });",
        "in t1 n1:1:false t2 t3 n2:2:false t4 n3:3:true r:9:true n4:undefined:true",
    );
    both(
        "async function* g() { try { yield 1; } finally { log.push('cleanup'); } }
         var it = g();
         it.next().then(function (r) { log.push('v' + r.value); return it.return(4); })
           .then(function (r) { log.push('r' + r.value + r.done); });
         it.throw === undefined;",
        "v1 cleanup r4true",
    );
}

#[test]
fn for_await_over_a_sync_iterable_and_combinators() {
    both(
        "(async function () {
           for await (var x of [1, Promise.resolve(2), 3]) log.push('x' + x);
         })();
         Promise.all([1, Promise.resolve(2)]).then(function (v) { log.push('all' + v); });
         Promise.allSettled([Promise.reject(1), 2]).then(function (v) { log.push('settled' + v.map(function (r) { return r.status; })); });
         Promise.any([Promise.reject(1), Promise.resolve(5)]).then(function (v) { log.push('any' + v); });
         Promise.race([new Promise(function () {}), Promise.resolve(6)]).then(function (v) { log.push('race' + v); });",
        "x1 all1,2 settledrejected,fulfilled any5 race6 x2 x3",
    );
}

#[test]
fn subclasses_and_foreign_constructors_still_get_real_resolving_functions() {
    both(
        "class P extends Promise {}
         var p = P.resolve(1).then(function (v) { log.push('sub' + v); return v + 1; });
         log.push(p instanceof P);
         p.then(function (v) { log.push('next' + v); });
         var seen;
         function C(ex) { ex(function (v) { log.push('res' + v); }, function () {}); seen = ex; }
         Promise.resolve.call(C, 3);
         log.push(typeof seen, seen.length);",
        "true res3 function 2 sub1 next2",
    );
}

// ---- resolving functions script can see -----------------------------------------------------

#[test]
fn visible_resolving_functions_behave_as_before() {
    both(
        "var res, rej;
         var p = new Promise(function (a, b) { res = a; rej = b; throw 'ignored after resolve? no: before'; });
         p.catch(function (e) { log.push('threw:' + e); });
         var q = new Promise(function (a, b) { res = a; rej = b; });
         log.push(typeof res, res.length, JSON.stringify(res.name), Object.getPrototypeOf(res) === Function.prototype,
                  res === rej, Object.getOwnPropertyNames(res).join(','), 'prototype' in res);
         try { new res(); } catch (e) { log.push(e.constructor.name); }
         log.push(res(1), res(2), rej(3));
         q.then(function (v) { log.push('q' + v); });
         var w = Promise.withResolvers();
         log.push(typeof w.resolve, w.resolve.length, w.reject.length);
         w.reject(4); w.resolve(5);
         w.promise.catch(function (e) { log.push('w' + e); });
         var self = new Promise(function (a) { res = a; }); res(self);
         self.catch(function (e) { log.push(e.constructor.name); });",
        "function 1 \"\" true false length,name false TypeError    function 1 1 threw:ignored after resolve? no: before q1 w4 TypeError",
    );
}

// ---- weight ---------------------------------------------------------------------------------

fn objects_made(vm: &mut Vm, src: &str) -> u64 {
    let realm = vm.main_realm();
    let before = vm.heap.objects_allocated;
    vm.eval_script(realm, "weight.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"));
    vm.run_jobs(realm).unwrap();
    vm.heap.objects_allocated - before
}

#[test]
fn an_await_allocates_no_function_objects() {
    let mut vm = vm(false);
    let realm = vm.main_realm();
    vm.eval_script(
        realm,
        "setup.js",
        "var result = 0;
         async function step(i) { await null; return i; }
         async function loop(n) { var s = 0; for (var i = 0; i < n; i++) s += await step(i); result = s; }",
    )
    .unwrap();
    let made = objects_made(&mut vm, "loop(1000);");
    // Each step call needs its result promise and its suspended body; the
    // awaits themselves need nothing that script could see.
    assert!(made <= 2 * 1000 + 20, "{made} objects for 1000 iterations");
    assert_eq!(
        vm.eval_script(realm, "r.js", "result").unwrap(),
        ApiValue::Number((0..1000).sum::<i32>() as f64)
    );
}

#[test]
fn then_on_a_native_promise_allocates_only_the_result_promise() {
    let mut vm = vm(false);
    let realm = vm.main_realm();
    vm.eval_script(realm, "setup.js", "var c = 0; function inc() { c++; }").unwrap();
    let made = objects_made(
        &mut vm,
        "for (var i = 0; i < 1000; i++) Promise.resolve(i).then(inc);",
    );
    // Promise.resolve(i) and the promise `then` returns.
    assert!(made <= 2 * 1000 + 20, "{made} objects for 1000 thens");
    assert_eq!(vm.eval_script(realm, "c.js", "c").unwrap(), ApiValue::Number(1000.0));
}

#[test]
fn a_pending_then_chain_holds_one_object_per_link() {
    let mut vm = vm(false);
    let realm = vm.main_realm();
    vm.eval_script(realm, "setup.js", "var r; var head = new Promise(function (a) { r = a; }); function f(v) { return v + 1; }")
        .unwrap();
    vm.collect_garbage();
    let before = vm.memory_usage().object_count;
    vm.eval_script(realm, "chain.js", "var p = head; for (var i = 0; i < 1000; i++) p = p.then(f);")
        .unwrap();
    vm.collect_garbage();
    let held = vm.memory_usage().object_count - before;
    assert!(held <= 1000 + 20, "{held} live objects for 1000 pending links");
    vm.eval_script(realm, "go.js", "var out; r(1); p.then(function (v) { out = v; });").unwrap();
    vm.run_jobs(realm).unwrap();
    assert_eq!(vm.eval_script(realm, "out.js", "out").unwrap(), ApiValue::Number(1001.0));
}

// ---- collections while suspended ------------------------------------------------------------

#[test]
fn a_collection_at_every_safe_point_keeps_awaiting_bodies_alive() {
    let src = "var result = '';
         async function step(i) { var o = { i: i, s: 'v' + i }; await null; await Promise.resolve(); return o.s + o.i; }
         async function* gen(n) { for (var i = 0; i < n; i++) { await null; yield await step(i); } }
         (async function () {
           var parts = [];
           for await (var s of gen(30)) parts.push(s);
           var r = await Promise.all([step(100), step(101), new Promise(function (res) { res(step(102)); })]);
           result = parts.length + ':' + parts[29] + ':' + r.join(',');
         })();";
    let mut vm = vm(true);
    let realm = vm.main_realm();
    vm.eval_script(realm, "stress.js", src).unwrap();
    vm.run_jobs(realm).unwrap();
    assert_eq!(
        vm.eval_script(realm, "r.js", "result").unwrap(),
        ApiValue::String("30:v2929:v100100,v101101,v102102".into())
    );
}
