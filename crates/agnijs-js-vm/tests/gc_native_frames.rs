//! The collector runs while native frames are on the stack (TASK-100.16).
//!
//! Before this change `Vm::maybe_collect` returned at once under a native
//! frame, so one long synchronous call from a native callback (`forEach`,
//! `map`, `sort`, `replace`), a promise job, a host call or a module body
//! kept all of its garbage. Now a collection under a nested run loop
//! treats every heap value that existed when Rust entered that loop as a
//! root, which is everything a native could hold in a Rust local, so no
//! native needs per-site rooting.
//!
//! Two kinds of test: memory (garbage made under a native frame is
//! reclaimed while the frame is still on the stack) and rooting (what a
//! native holds across a callback survives, under a collection at every
//! safe point and at every 16th).

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, PropertyKey, Value};
use agnijs_js_vm::object::CallArgs;
use agnijs_js_vm::value::{Attrs, PropKey, Value as VmValue};
use agnijs_js_vm::vm::JsResult;
use agnijs_js_vm::Vm;

/// Live objects in the arena (collected or not).
fn live_objects(vm: &mut Vm, _a: &CallArgs) -> JsResult<VmValue> {
    Ok(VmValue::Number(vm.heap.objects.live() as f64))
}

fn gc_count(vm: &mut Vm, _a: &CallArgs) -> JsResult<VmValue> {
    Ok(VmValue::Number(vm.gc_count as f64))
}

/// `requestGc()`: what the host's `collect_garbage` does during a run.
fn request_gc(vm: &mut Vm, _a: &CallArgs) -> JsResult<VmValue> {
    vm.collect_garbage();
    Ok(VmValue::Undefined)
}

/// `holdAcross(arr, cb)`: a native that reads `arr[0]` into a Rust local,
/// calls back into JavaScript (which may unlink the element and allocate
/// freely), then reads a property of the held element. Every built-in
/// that calls back follows this pattern.
fn hold_across(vm: &mut Vm, a: &CallArgs) -> JsResult<VmValue> {
    let VmValue::Object(arr) = a.args[0] else { return vm.type_error("array") };
    let held = vm.get_prop(arr, PropKey::Index(0), a.args[0])?;
    let VmValue::Object(held_obj) = held else { return vm.type_error("object") };
    let fresh = vm.new_object();
    let tag = vm.key("tag");
    let fresh_tag = vm.str("fresh");
    vm.define_raw(fresh, tag, fresh_tag, Attrs::METHOD);
    // The callback runs in a nested run loop; collections may happen there.
    vm.call_value(a.args[1], VmValue::Undefined, &[])?;
    let v = vm.get_prop(held_obj, tag, held)?;
    let w = vm.get_prop(fresh, tag, VmValue::Object(fresh))?;
    let s = format!("{}:{}", vm.display(v), vm.display(w));
    Ok(vm.str(&s))
}

fn engine(stress_every: u32) -> Vm {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress_every != 0;
    vm.heap.stress_every = stress_every.max(1);
    let global = vm.realm().global;
    for (name, f, n) in [
        ("liveObjects", live_objects as fn(&mut Vm, &CallArgs) -> JsResult<VmValue>, 0),
        ("gcCount", gc_count, 0),
        ("requestGc", request_gc, 0),
        ("holdAcross", hold_across, 2),
    ] {
        let fo = vm.new_native(name, n, f);
        vm.define_raw_str(global, name, VmValue::Object(fo), Attrs::METHOD);
    }
    vm
}

fn run(vm: &mut Vm, src: &str) -> Value {
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "gc-native-frames.js", src)
        .unwrap_or_else(|e| panic!("{e}: {src}"));
    vm.run_jobs(realm).unwrap();
    v
}

fn num(v: Value) -> f64 {
    v.as_number().unwrap_or_else(|| panic!("expected a number, got {v:?}"))
}

// ---- memory: garbage under a native frame is reclaimed -------------------

/// 150k iterations allocate 300k objects (about 60 MiB), several times the
/// collection threshold. Without a collection under the frame every one of
/// them is still live when the callback ends.
const WORK: &str = "function work() { let keep = 0;
    for (let i = 0; i < 150000; i++) { const o = { a: i, b: [i, i + 1] }; keep += o.b.length; }
    return keep; }";
const LIVE_LIMIT: f64 = 120_000.0;

fn assert_collected_under(src: &str) {
    let mut vm = engine(0);
    let v = run(&mut vm, &format!("{WORK}\nvar live = -1, sum = 0;\n{src}\nsum === 300000 ? live : -sum"));
    let live = num(v);
    assert!(live >= 0.0, "work() did not run as expected: {live}");
    assert!(live < LIVE_LIMIT, "{live} objects still live under the native frame: {src}");
}

#[test]
fn garbage_made_in_a_foreach_callback_is_collected_while_foreach_runs() {
    assert_collected_under("[0].forEach(() => { sum = work(); live = liveObjects(); });");
}

#[test]
fn garbage_made_in_map_filter_reduce_and_sort_callbacks_is_collected() {
    assert_collected_under("[0].map(() => { sum = work(); live = liveObjects(); return 1; });");
    assert_collected_under("[0].filter(() => { sum = work(); live = liveObjects(); return true; });");
    assert_collected_under("[0, 1].reduce((a, b) => { sum = work(); live = liveObjects(); return a + b; });");
    assert_collected_under("[2, 1].sort((a, b) => { sum = work(); live = liveObjects(); return a - b; });");
}

#[test]
fn garbage_made_in_a_replace_callback_and_a_reviver_is_collected() {
    assert_collected_under("'x'.replace(/x/, () => { sum = work(); live = liveObjects(); return 'y'; });");
    assert_collected_under("JSON.parse('[1]', (k, v) => { if (k === '0') { sum = work(); live = liveObjects(); } return v; });");
}

#[test]
fn garbage_made_in_array_from_and_map_foreach_callbacks_is_collected() {
    assert_collected_under("Array.from([0], () => { sum = work(); live = liveObjects(); return 1; });");
    assert_collected_under("new Map([[1, 2]]).forEach(() => { sum = work(); live = liveObjects(); });");
}

#[test]
fn garbage_made_in_a_getter_called_from_a_native_is_collected() {
    // `Object.assign` reads through a getter: a nested run loop two natives deep.
    assert_collected_under(
        "Object.assign({}, { get p() { sum = work(); live = liveObjects(); return 1; } });",
    );
}

#[test]
fn garbage_made_inside_a_promise_job_is_collected_while_the_job_runs() {
    assert_collected_under("Promise.resolve().then(() => { sum = work(); live = liveObjects(); });");
}

#[test]
fn garbage_made_inside_a_host_call_is_collected_while_the_call_runs() {
    let mut vm = engine(0);
    run(&mut vm, &format!("{WORK}\nvar live = -1; function tick() {{ var s = work(); live = liveObjects(); return s; }}"));
    let realm = vm.main_realm();
    let global = vm.global_object(realm).unwrap();
    let tick = Engine::get(&mut vm, realm, &global, &PropertyKey::from("tick")).unwrap();
    assert_eq!(
        Engine::call(&mut vm, realm, &tick, &Value::Undefined, &[]).unwrap(),
        Value::Number(300000.0)
    );
    let live = num(run(&mut vm, "live"));
    assert!(live < LIVE_LIMIT, "{live} objects still live inside the host call");
}

#[test]
fn garbage_made_at_module_top_level_is_collected_while_the_module_runs() {
    let host = TestHost::new();
    host.add_module(
        "work.mjs",
        &format!("{WORK}\nglobalThis.sum = work(); globalThis.live = liveObjects();"),
    );
    let mut vm = Vm::new(host);
    Vm::create_realm(&mut vm);
    let global = vm.realm().global;
    let fo = vm.new_native("liveObjects", 0, live_objects);
    vm.define_raw_str(global, "liveObjects", VmValue::Object(fo), Attrs::METHOD);
    let realm = vm.main_realm();
    vm.load_module(realm, "work.mjs").unwrap();
    vm.run_jobs(realm).unwrap();
    let v = run(&mut vm, "sum === 300000 ? live : -1");
    let live = num(v);
    assert!(live >= 0.0);
    assert!(live < LIVE_LIMIT, "{live} objects still live during module evaluation");
}

#[test]
fn a_requested_collection_runs_at_the_next_safe_point_under_a_native_frame() {
    let mut vm = engine(0);
    let v = run(
        &mut vm,
        "var before = -1, after = -1;
         [0].forEach(() => { before = gcCount(); requestGc(); for (var i = 0; i < 3; i++); after = gcCount(); });
         after - before",
    );
    assert_eq!(num(v), 1.0);
}

// ---- rooting: what natives hold across callbacks survives -----------------

/// Run `src` with no stress, a collection at every safe point and at every
/// 16th; it must evaluate to `expected`.
fn check(src: &str, expected: &str) {
    for stress in [0, 1, 16] {
        let mut vm = engine(stress);
        let v = run(&mut vm, src);
        assert_eq!(v, Value::string(expected), "stress={stress}: {src}");
    }
}

const GARBAGE: &str = "function garbage() { var junk = []; for (var i = 0; i < 40; i++) junk.push({ i: i, s: 'g' + i }); return junk.length; }";

#[test]
fn a_native_holding_an_element_the_callback_unlinks_still_reads_it() {
    check(
        &format!("{GARBAGE}\nvar arr = [{{ tag: 'held' }}];\nholdAcross(arr, () => {{ arr.length = 0; garbage(); garbage(); }})"),
        "held:fresh",
    );
}

#[test]
fn sort_keeps_its_elements_while_the_comparator_empties_the_array_and_allocates() {
    check(
        &format!("{GARBAGE}\nvar a = []; for (var i = 0; i < 40; i++) a.push({{ k: (i * 7) % 40, s: 'v' + i }});
         var sorted = a.slice().sort((x, y) => {{ a.length = 0; garbage(); return x.k - y.k; }});
         sorted.length + ':' + sorted.every((o, i) => o.k === i && o.s === 'v' + ((i * 23) % 40))"),
        "40:true",
    );
}

#[test]
fn map_filter_flatmap_and_from_keep_their_results_across_allocating_callbacks() {
    check(
        &format!("{GARBAGE}\nvar src = []; for (var i = 0; i < 50; i++) src.push(i);
         var m = src.map(i => {{ garbage(); return {{ i, s: 'm' + i }}; }});
         var f = m.filter(o => {{ garbage(); return o.i % 2 === 0; }});
         var fm = f.flatMap(o => {{ garbage(); return [o.s, o.s + '!']; }});
         var af = Array.from(fm, s => {{ garbage(); return s.length; }});
         m.length + ':' + f.length + ':' + fm.join(',').length + ':' + af.reduce((a, b) => {{ garbage(); return a + b; }}, 0)"),
        "50:25:214:165",
    );
}

#[test]
fn replace_split_and_matchall_keep_their_strings_across_allocating_callbacks() {
    check(
        &format!("{GARBAGE}\nvar s = 'a1b22c333d'.repeat(20);
         var r = s.replace(/\\d+/g, (m, off) => {{ garbage(); return '<' + m.length + '@' + off + '>'; }});
         var parts = s.split(/\\d+/);
         var ms = [...s.matchAll(/\\d+/g)].map(m => {{ garbage(); return m[0]; }});
         r.length + ':' + parts.length + ':' + ms.length + ':' + ms.join('').length"),
        "467:61:60:120",
    );
}

#[test]
fn json_stringify_and_parse_keep_their_state_across_allocating_callbacks() {
    check(
        &format!("{GARBAGE}\nvar o = {{ a: [1, {{ b: 'x' }}, 3], c: {{ d: 'y', toJSON() {{ garbage(); return {{ d2: this.d }}; }} }} }};
         var text = JSON.stringify(o, (k, v) => {{ garbage(); return v; }});
         var back = JSON.parse(text, (k, v) => {{ garbage(); return typeof v === 'number' ? v * 2 : v; }});
         text + '|' + back.a[0] + back.a[2] + back.c.d2"),
        "{\"a\":[1,{\"b\":\"x\"},3],\"c\":{\"d2\":\"y\"}}|26y",
    );
}

#[test]
fn object_assign_entries_and_fromentries_keep_keys_across_getters_and_proxy_traps() {
    check(
        &format!("{GARBAGE}\nvar src = {{ get a() {{ garbage(); return 1; }}, get b() {{ garbage(); return 2; }}, c: 3 }};
         var p = new Proxy({{ x: 1, y: 2 }}, {{ ownKeys(t) {{ garbage(); return ['k' + (garbage() + 260), 'y', 'x']; }},
             getOwnPropertyDescriptor(t, k) {{ garbage(); return {{ value: k + k, enumerable: true, configurable: true }}; }},
             get(t, k) {{ garbage(); return k + '!'; }} }});
         var t = Object.assign({{}}, src, p);
         var e = Object.entries(p).map(([k, v]) => {{ garbage(); return k + '=' + v; }});
         var fe = Object.fromEntries(Object.entries(src));
         JSON.stringify(t) + '|' + e.join(',') + '|' + JSON.stringify(fe)"),
        "{\"a\":1,\"b\":2,\"c\":3,\"k300\":\"k300!\",\"y\":\"y!\",\"x\":\"x!\"}|k300=k300!,y=y!,x=x!|{\"a\":1,\"b\":2,\"c\":3}",
    );
}

#[test]
fn map_and_set_builders_and_foreach_keep_entries_across_allocating_iterators() {
    check(
        &format!("{GARBAGE}\nfunction* pairs() {{ for (var i = 0; i < 30; i++) {{ garbage(); yield [{{ k: i }}, 'v' + i]; }} }}
         var m = new Map(pairs());
         var seen = [];
         m.forEach((v, k) => {{ garbage(); if (k.k % 10 === 0) m.delete(k); seen.push(k.k + v); }});
         var s = new Set((function* () {{ for (var i = 0; i < 20; i++) {{ garbage(); yield 's' + (i % 7); }} }})());
         m.size + ':' + seen.length + ':' + seen[29] + ':' + s.size + ':' + [...s].join('')"),
        "27:30:29v29:7:s0s1s2s3s4s5s6",
    );
}

#[test]
fn promise_combinators_keep_their_values_across_allocating_thenables() {
    check(
        &format!("{GARBAGE}\nvar out = 'pending';
         var thenables = [];
         for (var i = 0; i < 20; i++) thenables.push({{ then(res) {{ garbage(); res({{ i: this.i, s: 'p' + this.i }}); }}, i }});
         Promise.all(thenables).then(vs => {{ garbage(); out = vs.length + ':' + vs.map(v => v.s).join('').length; }});
         Promise.allSettled(thenables).then(rs => {{ garbage(); out += ':' + rs.filter(r => r.status === 'fulfilled').length; }});
         Promise.race(thenables).then(v => {{ garbage(); out += ':' + v.s; }});
         Promise.any(thenables).then(v => {{ garbage(); out += ':' + v.i; }});
         out"),
        "pending",
    );
    // The reactions ran as jobs after the script; read the result from a second script.
    for stress in [0, 1, 16] {
        let mut vm = engine(stress);
        run(
            &mut vm,
            &format!("{GARBAGE}\nvar out = '';
             var thenables = [];
             for (var i = 0; i < 20; i++) thenables.push({{ then(res) {{ garbage(); res({{ i: this.i, s: 'p' + this.i }}); }}, i }});
             Promise.all(thenables).then(vs => {{ garbage(); out = vs.length + ':' + vs.map(v => v.s).join('').length; }});
             Promise.allSettled(thenables).then(rs => {{ garbage(); out += ':' + rs.filter(r => r.status === 'fulfilled').length; }});
             Promise.race(thenables).then(v => {{ garbage(); out += ':' + v.s; }});
             Promise.any(thenables).then(v => {{ garbage(); out += ':' + v.i; }});"),
        );
        assert_eq!(run(&mut vm, "out"), Value::string("20:50:20:p0:0"), "stress={stress}");
    }
}

#[test]
fn reflect_and_proxy_traps_keep_target_and_arguments_across_allocating_handlers() {
    check(
        &format!("{GARBAGE}\nvar log = [];
         var target = {{ a: 1 }};
         var p = new Proxy(target, {{
           get(t, k, r) {{ garbage(); log.push('get:' + String(k)); return Reflect.get(t, k, r); }},
           set(t, k, v, r) {{ garbage(); return Reflect.set(t, k, v, r); }},
           has(t, k) {{ garbage(); return k in t; }},
           deleteProperty(t, k) {{ garbage(); return delete t[k]; }},
           apply(t, th, args) {{ garbage(); return args.length; }},
         }});
         p.b = {{ s: 'b' }}; var r = p.a + ':' + ('b' in p) + ':' + delete p.a + ':' + p.b.s;
         var f = new Proxy(function () {{}}, {{ apply(t, th, args) {{ garbage(); return args.map(a => a.s).join(''); }} }});
         r + ':' + f({{ s: 'x' }}, {{ s: 'y' }}) + ':' + Reflect.apply(f, null, [{{ s: 'z' }}]) + ':' + log.length"),
        "1:true:true:b:xy:z:2",
    );
}

#[test]
fn string_and_number_conversions_keep_their_operands_across_allocating_valueof() {
    check(
        &format!("{GARBAGE}\nvar objs = []; for (var i = 0; i < 20; i++) objs.push({{ i, valueOf() {{ garbage(); return this.i; }}, toString() {{ garbage(); return 's' + this.i; }} }});
         var sum = 0; for (var o of objs) sum += o * 2;
         var joined = objs.join('');
         var cmp = objs.slice().sort((a, b) => {{ garbage(); return b - a; }}).map(o => o.i)[0];
         var tpl = `${{objs[3]}}-${{objs[4]}}`;
         sum + ':' + joined.length + ':' + cmp + ':' + tpl + ':' + Math.max(...objs) + ':' + String(objs[5]).length"),
        "380:50:19:s3-s4:19:2",
    );
}

#[test]
fn bound_functions_apply_and_construct_keep_their_arguments_across_allocating_callees() {
    check(
        &format!("{GARBAGE}\nfunction F(a, b) {{ garbage(); this.s = a.s + b.s; }}
         var B = F.bind(null, {{ s: 'x' }});
         var o = new B({{ s: 'y' }});
         var r = Reflect.construct(F, [{{ s: 'p' }}, {{ s: 'q' }}]);
         var ap = F.apply({{}}, [{{ s: 'm' }}, {{ s: 'n' }}]);
         o.s + ':' + r.s + ':' + (ap === undefined)"),
        "xy:pq:true",
    );
}

#[test]
fn iterator_helpers_and_spread_keep_their_records_across_allocating_steps() {
    check(
        &format!("{GARBAGE}\nfunction* g() {{ for (var i = 0; i < 12; i++) {{ garbage(); yield {{ i, s: 'i' + i }}; }} }}
         var a = [...g()].map(o => o.s).join('').length;
         var b = g().map(o => {{ garbage(); return o.i * 2; }}).filter(x => {{ garbage(); return x % 4 === 0; }}).take(3).toArray().join(',');
         var c = Math.max(...g().map(o => o.i));
         var d = Array.from(g()).at(-1).s;
         a + ':' + b + ':' + c + ':' + d"),
        "26:0,4,8:11:i11",
    );
}

#[test]
fn typed_array_and_array_buffer_callbacks_keep_their_buffers_across_allocation() {
    check(
        &format!("{GARBAGE}\nvar ta = new Float64Array(16).map((v, i) => {{ garbage(); return i / 2; }});
         var sorted = Int32Array.from([5, 3, 9, 1], x => {{ garbage(); return x * 10; }}).sort((a, b) => {{ garbage(); return b - a; }});
         var sum = ta.reduce((a, b) => {{ garbage(); return a + b; }}, 0);
         sum + ':' + Array.from(sorted).join(',') + ':' + ta.filter(x => {{ garbage(); return x > 6; }}).length"),
        "60:90,50,30,10:3",
    );
}

#[test]
fn generator_and_async_resumption_from_natives_keeps_values_across_allocation() {
    for stress in [0, 1, 16] {
        let mut vm = engine(stress);
        run(
            &mut vm,
            &format!("{GARBAGE}\nvar out = '';
             async function f(o) {{ garbage(); await null; garbage(); return o.s + '!'; }}
             async function* ag() {{ for (var i = 0; i < 5; i++) {{ garbage(); yield {{ s: 'a' + i }}; }} }}
             (async () => {{
               var parts = [];
               for await (var v of ag()) {{ garbage(); parts.push(await f(v)); }}
               out = parts.join('');
             }})();"),
        );
        assert_eq!(run(&mut vm, "out"), Value::string("a0!a1!a2!a3!a4!"), "stress={stress}");
    }
}

#[test]
fn the_memory_limit_still_applies_under_a_native_frame() {
    let mut vm = engine(0);
    vm.heap.set_memory_limit(Some(16 * 1024 * 1024));
    let realm = vm.main_realm();
    // Live data grows past the limit inside a callback: an error, not a hang.
    let r = vm.eval_script(
        realm,
        "limit.js",
        "var keep = []; [0].forEach(() => { for (var i = 0; i < 2000000; i++) keep.push({ i: i, a: [i] }); }); keep.length",
    );
    assert!(r.is_err(), "the memory limit was not enforced under forEach");
    // Garbage (not live data) under the same limit is fine: the collector runs.
    let mut vm = engine(0);
    vm.heap.set_memory_limit(Some(16 * 1024 * 1024));
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "ok.js", &format!("{WORK}\n[0].map(() => work())[0]"))
        .unwrap_or_else(|e| panic!("garbage under a limit faulted: {e}"));
    assert_eq!(v, Value::Number(300000.0));
}
