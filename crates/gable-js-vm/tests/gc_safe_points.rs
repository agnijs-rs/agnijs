//! Garbage is collected when an embedder runs code from an event loop: from
//! promise jobs and from host calls (timer callbacks, event handlers). Every
//! job and every host call runs its function as a native frame, where the
//! collector is deferred, so without a safe point between jobs and before
//! each host call such an embedder never collects at all (found by the
//! TASK-100.10 benchmarks: memory grew without bound).

use gable_engine_api::conformance::TestHost;
use gable_engine_api::{Engine, PropertyKey, Value};
use gable_js_vm::Vm;

#[test]
fn garbage_made_by_promise_jobs_is_collected_while_the_queue_drains() {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    // 3000 jobs, each leaving about 2000 dead objects behind.
    let src = "var n = 0;
         function step() {
           var junk = [];
           for (var i = 0; i < 2000; i++) junk.push({ i: i });
           if (++n < 3000) Promise.resolve().then(step);
         }
         Promise.resolve().then(step);
         0";
    assert_eq!(vm.eval_script(realm, "gc-jobs.js", src).unwrap(), Value::Number(0.0));
    vm.run_jobs(realm).unwrap();
    // Read the heap before running any more script: a top-level script is
    // itself a safe point.
    let usage = vm.memory_usage();
    assert_eq!(vm.eval_script(realm, "n.js", "n").unwrap(), Value::Number(3000.0));
    assert!(
        usage.object_count < 500_000,
        "{} objects still live after 6M short-lived ones ({} bytes)",
        usage.object_count,
        usage.heap_bytes
    );
}

#[test]
fn garbage_made_by_host_calls_is_collected_between_calls() {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    let realm = vm.main_realm();
    let src = "var kept = [];
         function tick(k) {
           var junk = [];
           for (var i = 0; i < 2000; i++) junk.push({ i: i });
           kept.push(k);
           return junk.length;
         }
         0";
    vm.eval_script(realm, "gc-calls.js", src).unwrap();
    let global = vm.global_object(realm).unwrap();
    let tick = Engine::get(&mut vm, realm, &global, &PropertyKey::from("tick")).unwrap();
    // A held argument must survive the collections the calls trigger.
    let arg = vm.eval_script(realm, "arg.js", "({ label: 'held' })").unwrap();
    for i in 0..3000 {
        // Stress mode collects at every safe point: the first calls collect each time.
        vm.heap.stress = i < 50;
        assert_eq!(Engine::call(&mut vm, realm, &tick, &Value::Undefined, std::slice::from_ref(&arg)).unwrap(), Value::Number(2000.0));
    }
    let usage = vm.memory_usage();
    assert!(
        usage.object_count < 500_000,
        "{} objects still live after 6M short-lived ones ({} bytes)",
        usage.object_count,
        usage.heap_bytes
    );
    let check = "kept.length + ':' + kept.every(function (k) { return k === kept[0]; }) + ':' + kept[0].label";
    assert_eq!(vm.eval_script(realm, "check.js", check).unwrap(), Value::string("3000:true:held"));
}
