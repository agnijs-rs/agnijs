//! Bounded fuzzing of the C API (TASK-103.3).
//!
//! - Random sequences of API calls on live, freed, foreign and made-up ids
//!   and on random byte strings: nothing may panic (`AGNIJS_ERR_PANIC`) or
//!   crash, every status is a known one, and the host handle count matches
//!   the handles the test still owns.
//! - Mutated scripts, evaluated through the C API and directly through the
//!   Rust engine: the outcome (value as text, or exception message) must
//!   match.
//! - Mutated bytecode: corrupt input is refused or runs, never crashes.
//!
//! Bounded by default so it runs in CI; `AGNIJS_CAPI_FUZZ_ITERS` scales the
//! work and `AGNIJS_CAPI_FUZZ_SEED` picks the seed (printed on failure).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic, clippy::field_reassign_with_default)]

mod common;

use agnijs_capi::*;
use agnijs_engine_api::{Engine, Host};
use common::*;
use std::cell::Cell;
use std::ffi::{c_char, c_void};
use std::ptr;
use std::rc::Rc;

/// xorshift64*, so a seed reproduces a run on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

fn iterations(default: usize) -> usize {
    let scale = std::env::var("AGNIJS_CAPI_FUZZ_ITERS").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
    default * scale.max(1)
}

fn seed() -> u64 {
    std::env::var("AGNIJS_CAPI_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(0x5EED_CAFE_F00D_0001)
}

fn known(st: AgnijsStatus) -> bool {
    (AGNIJS_ERR_NOT_FOUND..=AGNIJS_OUT_OF_MEMORY).contains(&st) && st != AGNIJS_ERR_PANIC
}

// ---- interrupt: a poll budget, identical for both engines ----

const POLL_BUDGET: u32 = 400;

unsafe extern "C-unwind" fn poll_budget(ud: *mut c_void) -> i32 {
    let polls = unsafe { &*(ud as *const Cell<u32>) };
    polls.set(polls.get() + 1);
    i32::from(polls.get() > POLL_BUDGET)
}

unsafe extern "C-unwind" fn zero_clock(_: *mut c_void) -> f64 {
    0.0
}

unsafe extern "C-unwind" fn echo(e: AgnijsEngine, call: *const AgnijsCallInfo, _: *mut c_void, result: *mut AgnijsValue) -> AgnijsStatus {
    let call = unsafe { &*call };
    if call.argc == 0 {
        return unsafe { agnijs_new_undefined(e, result) };
    }
    unsafe { agnijs_value_dup(e, *call.args, result) }
}

fn fuzz_engine(polls: &Cell<u32>) -> AgnijsEngine {
    let mut o = options();
    o.memory_limit = 64 << 20;
    o.interrupt = Some(poll_budget);
    o.now_ms = Some(zero_clock);
    o.user_data = polls as *const Cell<u32> as *mut c_void;
    engine_with(&o)
}

// ---- random API call sequences ----

struct Pool {
    live: Vec<AgnijsValue>,
    dead: Vec<AgnijsValue>,
}

impl Pool {
    fn any(&self, rng: &mut Rng, foreign: AgnijsValue) -> AgnijsValue {
        match rng.below(8) {
            0 => 0,
            1 => rng.next(),
            2 if !self.dead.is_empty() => *rng.pick(&self.dead),
            3 => foreign,
            4 if !self.live.is_empty() => {
                // A live id with its generation or tag changed.
                let v = *rng.pick(&self.live);
                v ^ (1u64 << (32 + rng.below(32)))
            }
            _ if !self.live.is_empty() => *rng.pick(&self.live),
            _ => rng.next(),
        }
    }
    fn adopt(&mut self, st: AgnijsStatus, v: AgnijsValue) {
        if st == AGNIJS_OK && v != 0 {
            self.live.push(v);
        }
    }
}

fn random_bytes(rng: &mut Rng) -> Vec<u8> {
    let words: &[&[u8]] = &[b"x", b"length", b"constructor", b"__proto__", b"0", b"4294967295", b"", b"\xff\xfe", b"\xed\xa0\x80", b"a\0b", b"toString", b"then"];
    if rng.chance(2) {
        return rng.pick(words).to_vec();
    }
    (0..rng.below(12)).map(|_| rng.next() as u8).collect()
}

#[test]
fn random_call_sequences_never_panic_and_keep_the_handle_count() {
    let mut rng = Rng(seed());
    let polls = Cell::new(0);
    let other = engine();
    let mut foreign = 0;
    unsafe { agnijs_new_object(other, realm(other), &mut foreign) };
    let sequences = iterations(60);
    for seq in 0..sequences {
        let e = fuzz_engine(&polls);
        let r = realm(e);
        let mut pool = Pool { live: Vec::new(), dead: Vec::new() };
        let mut buffers: Vec<AgnijsBuffer> = Vec::new();
        for step in 0..120 {
            polls.set(0);
            let ctx = format!("seed {} sequence {seq} step {step}", seed());
            let a = pool.any(&mut rng, foreign);
            let b = pool.any(&mut rng, foreign);
            let c = pool.any(&mut rng, foreign);
            let bytes = random_bytes(&mut rng);
            let realm_arg = match rng.below(6) {
                0 => 0,
                1 => rng.next(),
                _ => r,
            };
            let mut out: AgnijsValue = 0;
            let mut n32: i32 = 0;
            let st = unsafe {
                match rng.below(30) {
                    0 => {
                        let st = agnijs_new_number(e, rng.next() as f64, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    1 => {
                        let st = agnijs_new_string_utf8(e, bytes.as_ptr().cast(), bytes.len(), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    2 => {
                        let units: Vec<u16> = (0..rng.below(6)).map(|_| rng.next() as u16).collect();
                        let st = agnijs_new_string_utf16(e, units.as_ptr(), units.len(), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    3 => {
                        let st = agnijs_new_object(e, realm_arg, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    4 => {
                        let items = [a, b, c];
                        let st = agnijs_new_array(e, realm_arg, items.as_ptr(), rng.below(4), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    5 => {
                        let st = agnijs_value_dup(e, a, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    6 => {
                        let st = agnijs_value_free(e, a);
                        if st == AGNIJS_OK {
                            let i = pool.live.iter().position(|&v| v == a).expect("freed a value the test did not own");
                            pool.live.swap_remove(i);
                            pool.dead.push(a);
                        }
                        st
                    }
                    7 => agnijs_value_type(e, a, &mut n32),
                    8 => {
                        let mut d = 0.0;
                        agnijs_value_get_number(e, a, &mut d)
                    }
                    9 => {
                        let mut buf = [0u8; 8];
                        let mut needed = 0;
                        agnijs_string_to_utf8(e, a, buf.as_mut_ptr().cast(), rng.below(9), &mut needed)
                    }
                    10 => {
                        let mut buf = [0u16; 8];
                        let mut needed = 0;
                        agnijs_string_to_utf16(e, a, buf.as_mut_ptr(), rng.below(9), &mut needed)
                    }
                    11 => {
                        let st = agnijs_to_string(e, realm_arg, a, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    12 => {
                        let mut d = 0.0;
                        agnijs_to_number(e, realm_arg, a, &mut d)
                    }
                    13 => {
                        let st = agnijs_get(e, realm_arg, a, bytes.as_ptr().cast(), bytes.len(), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    14 => agnijs_set(e, realm_arg, a, bytes.as_ptr().cast(), bytes.len(), b),
                    15 => {
                        let st = agnijs_get_by_value(e, realm_arg, a, b, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    16 => agnijs_set_by_value(e, realm_arg, a, b, c),
                    17 => agnijs_has(e, realm_arg, a, bytes.as_ptr().cast(), bytes.len(), &mut n32),
                    18 => agnijs_delete(e, realm_arg, a, bytes.as_ptr().cast(), bytes.len(), ptr::null_mut()),
                    19 => {
                        let st = agnijs_own_keys(e, realm_arg, a, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    20 => {
                        let args = [b, c];
                        let st = agnijs_call(e, realm_arg, a, if rng.chance(2) { 0 } else { b }, args.as_ptr(), rng.below(3), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    21 => {
                        let args = [b];
                        let st = agnijs_construct(e, realm_arg, a, args.as_ptr(), rng.below(2), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    22 => {
                        let st = agnijs_new_function(e, realm_arg, bytes.as_ptr().cast(), bytes.len(), Some(echo), ptr::null_mut(), None, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    23 => {
                        let mut state = 0;
                        let st = agnijs_promise_state(e, realm_arg, a, &mut state, &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    24 => agnijs_throw(e, a),
                    25 => {
                        let mut ran = 0;
                        agnijs_run_jobs(e, realm_arg, &mut ran)
                    }
                    26 => agnijs_collect_garbage(e),
                    27 => {
                        let srcs = ["({a: 1})", "[1, 2, 3]", "(x => x * 2)", "Promise.resolve(1)", "Symbol('s')", "10n ** 20n", "new Proxy({}, {})", "throw new Error('e')", "for(;;){}", "'x'.repeat(1 << 20)"];
                        let src = rng.pick(&srcs);
                        let st = agnijs_eval_script(e, realm_arg, b"f.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), &mut out);
                        pool.adopt(st, out);
                        st
                    }
                    28 => {
                        let mut buf = 0;
                        let src = "1 + 1";
                        let st = agnijs_compile_script(e, realm_arg, b"c.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), &mut buf);
                        if st == AGNIJS_OK {
                            buffers.push(buf);
                        }
                        st
                    }
                    _ => {
                        let buf = if buffers.is_empty() || rng.chance(3) { rng.next() } else { buffers.swap_remove(rng.below(buffers.len())) };
                        agnijs_buffer_free(e, buf)
                    }
                }
            };
            assert!(known(st), "{ctx}: status {st}: {}", last_error());
            if st == AGNIJS_EXCEPTION {
                // Taken or not, the next call replaces it; take it half the
                // time.
                if rng.chance(2) {
                    take_exception(e);
                }
            }
        }
        // Every value and buffer the test owns is still live; nothing else
        // is.
        let mut m = AgnijsMemoryUsage::default();
        m.struct_size = std::mem::size_of::<AgnijsMemoryUsage>() as u32;
        let mut ex = AgnijsException::default();
        ex.struct_size = std::mem::size_of::<AgnijsException>() as u32;
        if unsafe { agnijs_take_exception(e, &mut ex) } == AGNIJS_OK {
            for b in [ex.name, ex.message, ex.stack] {
                agnijs_buffer_free(e, b);
            }
            agnijs_value_free(e, ex.value);
        }
        assert_eq!(unsafe { agnijs_memory_usage(e, &mut m) }, AGNIJS_OK);
        assert_eq!(m.host_handles, pool.live.len() + buffers.len(),"seed {} sequence {seq}", seed());
        for v in pool.live.drain(..) {
            free(e, v);
        }
        for b in buffers.drain(..) {
            assert_eq!(agnijs_buffer_free(e, b), AGNIJS_OK);
        }
        free_engine(e);
    }
    free(other, foreign);
    free_engine(other);
}

// ---- mutated scripts, against the Rust engine ----

const CORPUS: &[&str] = &[
    "1 + 2 * 3",
    "let a = [3, 1, 2]; a.sort(); a.join(',')",
    "const o = {x: 1, y: [2, 3]}; JSON.stringify(o)",
    "JSON.parse('{\"a\":[1,2,{\"b\":null}]}').a[2].b",
    "'hello world'.replace(/o/g, '0').toUpperCase()",
    "class A { #x = 1; get x() { return this.#x } } new A().x",
    "function* g() { yield 1; yield 2 } [...g()].length",
    "(() => { try { throw new TypeError('t') } catch (e) { return e.name } })()",
    "let s = 0; for (let i = 0; i < 10; i++) s += i; s",
    "const {a, ...rest} = {a: 1, b: 2, c: 3}; Object.keys(rest).join()",
    "`${1 + 1} and ${'x'.repeat(3)}`",
    "new Map([[1, 'a'], [2, 'b']]).get(2)",
    "[...new Set([1, 1, 2, 3])].reduce((p, c) => p + c, 0)",
    "(2n ** 64n).toString(16)",
    "new Uint8Array([1, 2, 300])[2]",
    "Object.getOwnPropertyNames(Math).length > 10",
    "Symbol('d').description",
    "new Date(0).toISOString()",
    "/(\\d+)-(\\d+)/.exec('10-20')[2]",
    "label: for (const x of [1, 2]) { for (const y of [3]) { continue label } } 'ok'",
    "Reflect.ownKeys(new Proxy({a: 1}, {})).join()",
    "Array.from({length: 5}, (_, i) => i * i).at(-1)",
    "(function () { return typeof arguments })()",
    "parseInt('ff', 16) + parseFloat('1.5e1')",
    "[1, [2, [3, [4]]]].flat(Infinity).length",
    "Object.entries({a: 1, b: 2}).map(([k, v]) => k + v).join('|')",
    "String.raw`a\\nb`.length",
    "Number.MAX_SAFE_INTEGER % 7",
    "async function f() { return 1 } typeof f()",
    "'abc'.padStart(6, '-') + 'x'.codePointAt(0)",
];

const TOKENS: &[&str] = &[
    "(", ")", "{", "}", "[", "]", ";", ",", "=>", "...", "async ", "await ", "yield ", "new ", "class ", "`", "\"", "'", "\\", "/", "0x", "1e309", "NaN", "null", "this", "super", "eval", "\u{2028}", "?.", "??", "#x", "let ", "var ", "delete ", "typeof ", "in ", "of ", "=", "+", "-", "*", "**", "<<", "!", "~", ":", "?", ".", "\n", " ",
];

fn mutate(rng: &mut Rng, src: &str) -> String {
    let mut s: Vec<char> = src.chars().collect();
    for _ in 0..1 + rng.below(3) {
        let len = s.len();
        match rng.below(5) {
            0 if len > 0 => {
                let at = rng.below(len);
                let n = 1 + rng.below(4.min(len - at));
                s.drain(at..at + n);
            }
            1 => {
                let at = rng.below(len + 1);
                let t: Vec<char> = rng.pick(TOKENS).chars().collect();
                s.splice(at..at, t);
            }
            2 if len > 1 => {
                let at = rng.below(len);
                let n = 1 + rng.below((len - at).min(8));
                let piece: Vec<char> = s[at..at + n].to_vec();
                let to = rng.below(len + 1);
                s.splice(to..to, piece);
            }
            3 if len > 0 => {
                let at = rng.below(len);
                s[at] = char::from_u32(32 + (rng.next() % 95) as u32).unwrap_or(' ');
            }
            _ => {
                let other: Vec<char> = rng.pick(CORPUS).chars().collect();
                let at = rng.below(len + 1);
                let n = rng.below(other.len());
                s.splice(at..at, other[n..].iter().copied().take(12));
            }
        }
    }
    s.into_iter().collect()
}

/// A Rust host matching the C engine's options: the same poll budget and
/// a frozen clock.
struct DiffHost {
    polls: Cell<u32>,
}

impl Host for DiffHost {
    fn interrupt(&self) -> bool {
        self.polls.set(self.polls.get() + 1);
        self.polls.get() > POLL_BUDGET
    }
    fn now_ms(&self) -> f64 {
        0.0
    }
}

/// The outcome through the C API: Some(text) for a value or an exception's
/// message, None when the run was cut short or the value has no text.
fn c_outcome(e: AgnijsEngine, src: &str) -> Option<String> {
    let (st, v) = eval_status(e, src);
    match st {
        AGNIJS_OK => {
            let r = realm(e);
            let mut s = 0;
            let st = unsafe { agnijs_to_string(e, r, v, &mut s) };
            free(e, v);
            if st != AGNIJS_OK {
                if st == AGNIJS_EXCEPTION {
                    take_exception(e);
                }
                return None;
            }
            let text = utf8(e, s);
            free(e, s);
            Some(format!("ok {text}"))
        }
        AGNIJS_EXCEPTION => Some(format!("throw {}", take_exception(e).1)),
        AGNIJS_INTERRUPTED | AGNIJS_OUT_OF_MEMORY => None,
        other => panic!("eval {src:?}: status {other}: {}", last_error()),
    }
}

fn rust_outcome(engine: &mut dyn Engine, src: &str) -> Option<String> {
    let realm = engine.main_realm();
    match engine.eval_script(realm, "test.js", src) {
        Ok(v) => match engine.to_string(realm, &v) {
            Ok(text) => Some(format!("ok {}", text)),
            Err(_) => None,
        },
        Err(agnijs_engine_api::Error::Exception(ex)) => Some(format!("throw {}", ex.message)),
        Err(_) => None,
    }
}

#[test]
fn mutated_scripts_match_the_rust_engine() {
    let mut rng = Rng(seed() ^ 0xD1FF);
    let polls = Cell::new(0);
    let e = fuzz_engine(&polls);
    let host = Rc::new(DiffHost { polls: Cell::new(0) });
    let mut rust = agnijs_js_vm::make_engine(host.clone());
    rust.set_memory_limit(Some(64 << 20));
    let mut compared = 0;
    for i in 0..iterations(400) {
        // Each case in its own block, so declarations do not collide.
        let src = format!("{{ {} }}", mutate(&mut rng, rng_pick_corpus(i)));
        polls.set(0);
        host.polls.set(0);
        let c = c_outcome(e, &src);
        let r = rust_outcome(&mut *rust, &src);
        if let (Some(c), Some(r)) = (&c, &r) {
            assert_eq!(c, r, "seed {} case {i}: {src:?}", seed());
            compared += 1;
        }
    }
    assert!(compared > iterations(400) / 4, "only {compared} cases compared");
    free_engine(e);
}

fn rng_pick_corpus(i: usize) -> &'static str {
    CORPUS[i % CORPUS.len()]
}

#[test]
fn invalid_source_bytes_are_refused() {
    let mut rng = Rng(seed() ^ 0xB17E);
    let polls = Cell::new(0);
    let e = fuzz_engine(&polls);
    let r = realm(e);
    for _ in 0..iterations(300) {
        let mut src = rng.pick(CORPUS).as_bytes().to_vec();
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(src.len() + 1);
            src.insert(at, rng.next() as u8);
        }
        polls.set(0);
        let mut v = 0;
        let st = unsafe { agnijs_eval_script(e, r, b"b.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), &mut v) };
        assert!(known(st), "status {st}: {}", last_error());
        match st {
            AGNIJS_OK => free(e, v),
            AGNIJS_EXCEPTION => {
                take_exception(e);
            }
            _ => {}
        }
        if std::str::from_utf8(&src).is_err() {
            assert_eq!(st, AGNIJS_ERR_INVALID_UTF8);
        }
    }
    free_engine(e);
}

#[test]
fn mutated_bytecode_is_refused_or_runs() {
    let mut rng = Rng(seed() ^ 0xB7EC);
    let polls = Cell::new(0);
    let e = fuzz_engine(&polls);
    let r = realm(e);
    let mut images = Vec::new();
    for src in CORPUS {
        let mut buf = 0;
        let st = unsafe { agnijs_compile_script(e, r, b"m.js".as_ptr().cast(), 4, src.as_ptr().cast(), src.len(), &mut buf) };
        assert_eq!(st, AGNIJS_OK, "{src}: {}", last_error());
        let mut p: *const u8 = ptr::null();
        let mut n = 0;
        assert_eq!(unsafe { agnijs_buffer_data(e, buf, &mut p, &mut n) }, AGNIJS_OK);
        images.push(unsafe { std::slice::from_raw_parts(p, n) }.to_vec());
        assert_eq!(agnijs_buffer_free(e, buf), AGNIJS_OK);
    }
    for _ in 0..iterations(600) {
        let mut img = rng.pick(&images).clone();
        match rng.below(4) {
            0 => {
                let n = rng.below(img.len() + 1);
                img.truncate(n);
            }
            1 => {
                for _ in 0..1 + rng.below(4) {
                    if !img.is_empty() {
                        let at = rng.below(img.len());
                        img[at] ^= 1 << rng.below(8);
                    }
                }
            }
            2 => {
                let at = rng.below(img.len() + 1);
                let extra: Vec<u8> = (0..rng.below(16)).map(|_| rng.next() as u8).collect();
                img.splice(at..at, extra);
            }
            _ => {
                if !img.is_empty() {
                    let at = rng.below(img.len());
                    img[at] = rng.next() as u8;
                }
            }
        }
        polls.set(0);
        let mut v = 0;
        let st = unsafe { agnijs_eval_bytecode(e, r, img.as_ptr(), img.len(), &mut v) };
        assert!(known(st), "status {st}: {}", last_error());
        match st {
            AGNIJS_OK => free(e, v),
            AGNIJS_EXCEPTION => {
                take_exception(e);
            }
            _ => {}
        }
    }
    free_engine(e);
}

// ---- module bytecode with imports (TASK-103.24) ----

thread_local! {
    /// Whether the load hook serves `mem:/flaky.js` right now.
    static FLAKY: Cell<bool> = const { Cell::new(false) };
}

unsafe extern "C-unwind" fn fuzz_resolve(_: *mut c_void, _: *const c_char, _: usize, spec: *const c_char, len: usize, sink: AgnijsSink) -> AgnijsStatus {
    let s = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(spec.cast::<u8>(), len) }).into_owned();
    let full = format!("mem:/{}", s.trim_start_matches("./"));
    unsafe { agnijs_sink_set_string(sink, full.as_ptr().cast(), full.len()) }
}

unsafe extern "C-unwind" fn fuzz_load(_: *mut c_void, name: *const c_char, len: usize, sink: AgnijsSink) -> AgnijsStatus {
    let s = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(name.cast::<u8>(), len) }).into_owned();
    let (kind, text) = match s.as_str() {
        "mem:/dep.js" => (AGNIJS_MODULE_SOURCE, "export const two = 2; export default 'd';"),
        "mem:/star.js" => (AGNIJS_MODULE_SOURCE, "export * from './dep.js'; export * as ns from './dep.js';"),
        "mem:/cyc_a.js" => (AGNIJS_MODULE_SOURCE, "import { b } from './cyc_b.js'; export const a = 1; export function fa() { return b; }"),
        "mem:/cyc_b.js" => (AGNIJS_MODULE_SOURCE, "import { a, fa } from './cyc_a.js'; export const b = 2; export const viaA = () => a + fa();"),
        "mem:/bad.js" => (AGNIJS_MODULE_SOURCE, "export const = ;"),
        "mem:/d.json" => (AGNIJS_MODULE_JSON, "{\"x\": [1, 2]}"),
        "mem:/tla.js" => (AGNIJS_MODULE_SOURCE, "export const late = await Promise.resolve(3);"),
        "mem:/throws.js" => (AGNIJS_MODULE_SOURCE, "throw new Error('dep threw');"),
        "mem:/flaky.js" if FLAKY.with(Cell::get) => (AGNIJS_MODULE_SOURCE, "import { missing } from './missing.js'; export const f = 7;"),
        _ => {
            let m = format!("no module {s}");
            unsafe { agnijs_sink_set_error(sink, m.as_ptr().cast(), m.len()) };
            return AGNIJS_ERR_NOT_FOUND;
        }
    };
    unsafe { agnijs_sink_set_module(sink, kind, text.as_ptr(), text.len()) }
}

/// Modules with every kind of request: named, namespace, default, star and
/// indirect re-exports, cycles, JSON, top-level await, a dependency that
/// throws, one that does not parse, one that is missing, one that comes
/// and goes, a missing export, dynamic import, and another image's name.
const MODULE_CORPUS: &[&str] = &[
    "import { two } from './dep.js'; globalThis.r0 = two;",
    "import * as s from './star.js'; globalThis.r1 = s.two + s.ns.two;",
    "import { a, fa } from './cyc_a.js'; import { viaA } from './cyc_b.js'; globalThis.r2 = viaA() + a + fa();",
    "import './bad.js';",
    "import data from './d.json' with { type: 'json' }; globalThis.r4 = data.x;",
    "import { late } from './tla.js'; globalThis.r5 = late + (await 1);",
    "import './throws.js';",
    "import { f } from './flaky.js'; globalThis.r7 = f;",
    "import { nope } from './dep.js'; globalThis.r8 = nope;",
    "import { x } from './missing.js'; globalThis.r9 = x;",
    "export { two as default } from './dep.js'; export * from './star.js'; globalThis.r10 = import.meta;",
    "import('./flaky.js').then(m => { globalThis.r11 = m.f; }, e => { globalThis.r11 = String(e); });",
    "import d from './img0.js'; globalThis.r12 = d;",
    "import './dep.js' with { type: 'json' };",
    "import dflt, * as all from './dep.js'; export { dflt }; export const both = [dflt, all.two];",
];

#[test]
fn module_bytecode_with_imports_never_panics() {
    let mut rng = Rng(seed() ^ 0x40D5);
    let polls = Cell::new(0u32);
    let mut o = options();
    o.memory_limit = 64 << 20;
    o.interrupt = Some(poll_budget);
    o.now_ms = Some(zero_clock);
    o.user_data = &polls as *const Cell<u32> as *mut c_void;
    o.resolve_module = Some(fuzz_resolve);
    o.load_module = Some(fuzz_load);
    let e = engine_with(&o);
    let r = realm(e);
    let images: Vec<(String, Vec<u8>)> = MODULE_CORPUS
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let name = format!("mem:/img{i}.js");
            let mut buf = 0;
            let st = unsafe { agnijs_compile_module(e, r, name.as_ptr().cast(), name.len(), src.as_ptr().cast(), src.len(), &mut buf) };
            assert_eq!(st, AGNIJS_OK, "{src}: {}", last_error());
            let mut p: *const u8 = ptr::null();
            let mut n = 0;
            assert_eq!(unsafe { agnijs_buffer_data(e, buf, &mut p, &mut n) }, AGNIJS_OK);
            let bytes = unsafe { std::slice::from_raw_parts(p, n) }.to_vec();
            assert_eq!(agnijs_buffer_free(e, buf), AGNIJS_OK);
            (name, bytes)
        })
        .collect();
    let deps = ["mem:/dep.js", "mem:/star.js", "mem:/cyc_a.js", "mem:/cyc_b.js", "mem:/flaky.js", "mem:/tla.js"];
    let (mut ran, mut threw) = (0, 0);
    for _ in 0..iterations(800) {
        polls.set(0);
        match rng.below(10) {
            0..=5 => {
                let (_, image) = rng.pick(&images);
                let mut img = image.clone();
                if rng.chance(4) && !img.is_empty() {
                    let at = rng.below(img.len());
                    img[at] ^= 1 << rng.below(8);
                }
                let mut v = 0;
                let st = unsafe { agnijs_eval_bytecode(e, r, img.as_ptr(), img.len(), &mut v) };
                assert!(known(st), "status {st}: {}", last_error());
                match st {
                    AGNIJS_OK => {
                        ran += 1;
                        free(e, v);
                    }
                    AGNIJS_EXCEPTION => {
                        threw += 1;
                        take_exception(e);
                    }
                    _ => {}
                }
            }
            6 => FLAKY.with(|f| f.set(!f.get())),
            7 => {
                let spec = if rng.chance(2) { rng.pick(&images).0.as_str() } else { *rng.pick(&deps) };
                let st = unsafe { agnijs_release_module(e, r, spec.as_ptr().cast(), spec.len(), ptr::null_mut()) };
                assert_eq!(st, AGNIJS_OK, "{}", last_error());
            }
            8 => {
                let st = unsafe { agnijs_run_jobs(e, r, ptr::null_mut()) };
                assert!(known(st), "status {st}: {}", last_error());
                if st == AGNIJS_EXCEPTION {
                    take_exception(e);
                }
            }
            _ => {
                let st = agnijs_collect_garbage(e);
                assert_eq!(st, AGNIJS_OK, "{}", last_error());
            }
        }
    }
    assert!(ran > 0 && threw > 0, "ran {ran}, threw {threw}");
    // Still a working engine.
    assert_eq!(eval_display(e, "6 * 7"), "42");
    free_engine(e);
}
