//! The target functions. See the crate docs for what a panic here means.

use agnijs_engine_api::{Engine, Error, Host, ModuleSource, Value};
use agnijs_js_parser::stack::{StackBudget, DEFAULT_BUDGET};
use agnijs_js_regex::{Flags, MatcherMode, Regex};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

static ACCEPTED: AtomicUsize = AtomicUsize::new(0);

/// Inputs that got past the component's front door (parsed, compiled,
/// loaded, decoded) since the last call; the runner reports it so that a
/// target whose mutants are all refused is noticed.
pub fn take_accepted() -> usize {
    ACCEPTED.swap(0, Ordering::Relaxed)
}

fn accepted() {
    ACCEPTED.fetch_add(1, Ordering::Relaxed);
}

// ---- the host every engine target runs under -----------------------------------------------------

/// Instructions a script may dispatch before it runs out of fuel.
const FUEL: u64 = 2_000_000;
/// Host interrupt polls (one per 4096 engine polls, and one per slice of
/// regex steps or native loop work) before the run is interrupted.
const POLLS: u32 = 256;
/// The engine's memory limit.
const MEMORY: usize = 64 << 20;

/// A host with a frozen clock, a fixed seed, a console that goes nowhere,
/// an interrupt after [`POLLS`] polls and modules served from memory, so
/// that an input always runs the same way.
pub struct FuzzHost {
    polls: Cell<u32>,
    modules: RefCell<HashMap<String, String>>,
}

impl FuzzHost {
    pub fn new() -> Rc<FuzzHost> {
        let host = FuzzHost { polls: Cell::new(0), modules: RefCell::new(HashMap::new()) };
        host.add_module("dep.mjs", "export const dep = 1; export default function f() { return 2; }");
        host.add_module("cycle.mjs", "import { dep } from 'dep.mjs'; export let c = dep; export { c as d };");
        host.add_module("tla.mjs", "export const v = await Promise.resolve(3);");
        Rc::new(host)
    }

    pub fn add_module(&self, specifier: &str, source: &str) {
        self.modules.borrow_mut().insert(specifier.to_string(), source.to_string());
    }

    /// Give the next run the whole poll budget again.
    pub fn rearm(&self) {
        self.polls.set(0);
    }
}

impl Host for FuzzHost {
    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        match self.modules.borrow().get(resolved) {
            Some(s) => Ok(ModuleSource::Source(s.clone())),
            None => Err(format!("module not found: {resolved}")),
        }
    }
    fn interrupt(&self) -> bool {
        let n = self.polls.get().saturating_add(1);
        self.polls.set(n);
        n > POLLS
    }
    fn now_ms(&self) -> f64 {
        1_700_000_000_000.0
    }
    fn random_seed(&self) -> u64 {
        0x5EED
    }
}

/// An engine under fuel, the interrupt and the memory limit.
fn engine(host: &Rc<FuzzHost>) -> Box<dyn Engine> {
    let mut e = agnijs_js_vm::make_engine(host.clone());
    limit(&mut *e);
    e
}

fn limit(e: &mut dyn Engine) {
    e.set_memory_limit(Some(MEMORY));
    e.set_fuel(Some(FUEL)).expect("fuel is supported");
}

/// Run the promise jobs a script left, under the same bounds.
fn drain_jobs(e: &mut dyn Engine) {
    let realm = e.main_realm();
    for _ in 0..8 {
        if !e.has_pending_jobs() || e.run_jobs(realm).is_err() {
            break;
        }
    }
}

/// Whatever the input did, the engine must not be poisoned (a panic
/// unwound through it) and must still run a script with fresh budgets.
fn assert_usable(e: &mut dyn Engine, host: &FuzzHost) {
    assert!(!e.is_poisoned(), "the engine was poisoned");
    e.collect_garbage();
    host.rearm();
    e.set_fuel(Some(FUEL)).expect("fuel is supported");
    let realm = e.main_realm();
    match e.eval_script(realm, "after.js", "6 * 7") {
        Ok(Value::Number(n)) => assert_eq!(n, 42.0, "the engine computes wrongly after the input"),
        // What the input left alive may hold the heap at the limit.
        Err(Error::OutOfMemory) => {}
        other => panic!("the engine is unusable after the input: {other:?}"),
    }
}

/// Input bytes as text. Invalid UTF-8 becomes U+FFFD.
fn text(data: &[u8]) -> String {
    String::from_utf8_lossy(data).into_owned()
}

// ---- parser and compiler ---------------------------------------------------------------------------

/// Parse as a script and as a module. Any outcome but a panic is fine.
pub fn parse(data: &[u8]) {
    let src = text(data);
    let _budget = StackBudget::enter(DEFAULT_BUDGET);
    let script = agnijs_js_parser::parse_script(&src).is_ok();
    if agnijs_js_parser::parse_module(&src).is_ok() || script {
        accepted();
    }
}

/// Compile as a script and as a module. What compiles must serialise to a
/// code tree and a bytecode container that decode back, and the tree must
/// re-encode to the same bytes.
pub fn compile(data: &[u8]) {
    use agnijs_js_compiler::serialize;
    use agnijs_js_vm::engine::{decode_bytecode, encode_bytecode};
    let src = text(data);
    let _budget = StackBudget::enter(DEFAULT_BUDGET);
    for module in [false, true] {
        let compiled = if module {
            agnijs_js_compiler::compile_module(&src, "fuzz.mjs")
        } else {
            agnijs_js_compiler::compile_script(&src, "fuzz.js")
        };
        let Ok(code) = compiled else { continue };
        accepted();
        let tree = serialize::encode(&code);
        let back = serialize::decode(&tree)
            .unwrap_or_else(|e| panic!("a compiled code tree does not decode (module: {module}): {e}"));
        assert!(serialize::encode(&back) == tree, "a decoded code tree re-encodes differently (module: {module})");
        let container = encode_bytecode(module, &code);
        if let Err(e) = decode_bytecode(&container, module) {
            panic!("a compiled bytecode container does not decode (module: {module}): {e}");
        }
    }
}

// ---- the engine interface ------------------------------------------------------------------------

/// Inputs that start with this line run as a module.
pub const MODULE_MARKER: &str = "//@module\n";

/// Run the input as a script, or as a module after [`MODULE_MARKER`],
/// then its jobs. Throwing, running out of fuel or memory and being
/// interrupted are all fine; a panic or an unusable engine is not.
pub fn eval(data: &[u8]) {
    let src = text(data);
    let host = FuzzHost::new();
    let mut e = engine(&host);
    let realm = e.main_realm();
    if let Some(module) = src.strip_prefix(MODULE_MARKER) {
        host.add_module("main.mjs", module);
        let _ = e.load_module(realm, "main.mjs");
    } else {
        let _ = e.eval_script(realm, "fuzz.js", &src);
    }
    drain_jobs(&mut *e);
    assert_usable(&mut *e, &host);
}

/// Run `body` with the input bound to the global `input` as a string; the
/// script's completion value must be `"ok"`, or the run must have stopped.
fn run_with_input(input: &str, body: &str) {
    let host = FuzzHost::new();
    let mut e = engine(&host);
    let realm = e.main_realm();
    let global = e.global_object(realm).expect("a global object");
    e.set(realm, &global, &"input".into(), &Value::string(input)).expect("set input");
    match e.eval_script(realm, "harness.js", body) {
        Ok(Value::String(s)) if s.to_string() == "ok" => {}
        Ok(other) => {
            let shown = e.to_string(realm, &other).unwrap_or_default();
            panic!("the harness reported a failure: {shown}");
        }
        Err(Error::OutOfFuel | Error::OutOfMemory | Error::Interrupted) => {}
        Err(other) => panic!("the harness failed: {other:?}"),
    }
    assert_usable(&mut *e, &host);
}

/// `JSON.parse`, then `JSON.stringify` of what it gives must be a fixed
/// point: parsing and stringifying it again gives the same text. A reviver
/// and an indent run on the same value.
pub fn json(data: &[u8]) {
    run_with_input(
        &text(data),
        r#"(() => {
            let v;
            try { v = JSON.parse(input); } catch (e) { return e instanceof SyntaxError ? "ok" : "parse threw " + e; }
            const a = JSON.stringify(v);
            if (a === undefined) return "ok";
            const b = JSON.stringify(JSON.parse(a));
            if (a !== b) return "not a fixed point: " + a + " then " + b;
            JSON.stringify(v, null, 2);
            JSON.parse(input, function (k, x) { return typeof x === "number" ? -x : x; });
            return "ok";
        })()"#,
    );
}

/// The Intl constructors, `supportedValuesOf` and the locale-sensitive
/// methods with a fuzzed locale and option values. Input lines: a locale,
/// an option value, another option value, then text to format.
pub fn intl(data: &[u8]) {
    run_with_input(
        &text(data),
        r#"(() => {
            const [loc = "", a = "", b = "", ...rest] = input.split("\n");
            const s = rest.join("\n");
            const n = Number(s.length ? s : a);
            const opts = { style: a, unit: b, currency: b.slice(0, 3), currencyDisplay: a, notation: b,
                           compactDisplay: a, signDisplay: b, roundingMode: a, numberingSystem: b,
                           maximumFractionDigits: s.length % 21, minimumIntegerDigits: 1 + s.length % 21,
                           timeZone: b, calendar: a, hourCycle: b, dateStyle: a, timeStyle: b,
                           era: a, year: b, month: a, weekday: b, fractionalSecondDigits: 1 + s.length % 3,
                           type: a, granularity: b, sensitivity: a, usage: b, caseFirst: a, collation: b,
                           numeric: s.length % 2 == 0, localeMatcher: a, fallback: b, languageDisplay: a };
            const attempt = (f) => { try { f(); } catch (e) { if (!(e instanceof Error)) throw e; } };
            const locales = [loc, [loc, a], undefined];
            for (const l of locales) {
                attempt(() => Intl.getCanonicalLocales(l));
                attempt(() => new Intl.Locale(loc, { calendar: a, region: b, script: a, language: b }).maximize().minimize().toString());
                attempt(() => new Intl.NumberFormat(l, opts).formatToParts(n));
                attempt(() => new Intl.NumberFormat(l, opts).formatRange(n, n + 1));
                attempt(() => new Intl.DateTimeFormat(l, opts).formatToParts(n));
                attempt(() => new Intl.DateTimeFormat(l, opts).formatRange(n, n * 2));
                attempt(() => new Intl.Collator(l, opts).compare(s, a));
                attempt(() => new Intl.PluralRules(l, opts).select(n));
                attempt(() => new Intl.RelativeTimeFormat(l, opts).format(n, b));
                attempt(() => new Intl.ListFormat(l, opts).format([a, b, s]));
                attempt(() => new Intl.DisplayNames(l, opts).of(b));
                attempt(() => [...new Intl.Segmenter(l, opts).segment(s)].length);
                attempt(() => s.localeCompare(a, l, opts));
                attempt(() => n.toLocaleString(l, opts));
                attempt(() => new Date(n).toLocaleString(l, opts));
                attempt(() => s.toLocaleUpperCase(l) + s.toLocaleLowerCase(l));
                attempt(() => s.normalize(a));
            }
            attempt(() => Intl.supportedValuesOf(a));
            return "ok";
        })()"#,
    );
}

/// The Temporal types with fuzzed ISO 8601 strings, units, rounding
/// options, `relativeTo` values, instants and time zones. Input lines: a
/// duration string (also tried as a time zone), a unit, a rounding mode, a
/// `relativeTo` or instant string, then a number.
pub fn temporal(data: &[u8]) {
    run_with_input(
        &text(data),
        r#"(() => {
            const [d = "", unit = "", mode = "", rel = "", ...rest] = input.split("\n");
            const n = Number(rest.join("\n"));
            const attempt = (f) => { try { f(); } catch (e) { if (!(e instanceof Error)) throw e; } };
            const relatives = [undefined, rel];
            attempt(() => Temporal.Duration.from(d).toString());
            attempt(() => Temporal.Duration.from(d).toString({ fractionalSecondDigits: n, smallestUnit: unit, roundingMode: mode }));
            attempt(() => Temporal.Duration.from({ [unit]: n, days: n }).toJSON());
            attempt(() => new Temporal.Duration(n, n, n, n, n, n, n, n, n, n).toString());
            attempt(() => new Temporal.Duration(0, 0, 0, n, n, n, n, n, n, n).negated().abs().toString());
            for (const relativeTo of relatives) {
                attempt(() => Temporal.Duration.from(d).round({ smallestUnit: unit, roundingMode: mode, roundingIncrement: n, relativeTo }).toString());
                attempt(() => Temporal.Duration.from(d).round({ largestUnit: unit, relativeTo }).toString());
                attempt(() => Temporal.Duration.from(d).total({ unit, relativeTo }));
                attempt(() => Temporal.Duration.compare(d, { [unit]: n }, { relativeTo }));
                attempt(() => Temporal.Duration.from(d).add({ [unit]: n }).subtract(d).toString());
            }
            attempt(() => Temporal.Instant.from(rel).round({ smallestUnit: unit, roundingMode: mode, roundingIncrement: n }).toString());
            attempt(() => Temporal.Instant.from(rel).toString({ timeZone: d, smallestUnit: unit, fractionalSecondDigits: n }));
            attempt(() => Temporal.Instant.from(rel).add(d).until(rel, { largestUnit: unit, smallestUnit: mode, roundingMode: mode }).toString());
            attempt(() => Temporal.Instant.fromEpochMilliseconds(n).since(rel, { largestUnit: unit }).toString());
            attempt(() => new Temporal.Instant(BigInt(Math.trunc(n) || 0) * 1000000n).toJSON());
            attempt(() => new Date(n).toTemporalInstant().toString({ timeZone: rel }));
            attempt(() => Temporal.PlainDate.from(rel, { overflow: mode }).toString({ calendarName: unit }));
            attempt(() => new Temporal.PlainDate(n, n, n, d).toString());
            attempt(() => Temporal.PlainDate.from({ year: n, monthCode: d, day: n, era: unit, eraYear: n, calendar: rel }, { overflow: mode }).toJSON());
            attempt(() => Temporal.PlainDate.from(rel).withCalendar(d).with({ [unit]: n }).toString());
            attempt(() => Temporal.PlainDate.from(rel).add(d, { overflow: mode }).until(rel, { largestUnit: unit, roundingMode: mode, roundingIncrement: n }).toString());
            attempt(() => Temporal.PlainDate.from(rel).since({ year: n, month: n, day: n }, { smallestUnit: unit }).toString());
            attempt(() => Temporal.Duration.from(d).total({ unit, relativeTo: Temporal.PlainDate.from(rel) }));
            attempt(() => Temporal.PlainTime.from(rel, { overflow: mode }).toString({ smallestUnit: unit, fractionalSecondDigits: n, roundingMode: mode }));
            attempt(() => new Temporal.PlainTime(n, n, n, n, n, n).toJSON());
            attempt(() => Temporal.PlainTime.from({ [unit]: n, second: n }, { overflow: mode }).with({ [d]: n }).toString());
            attempt(() => Temporal.PlainTime.from(rel).add(d).until(rel, { largestUnit: unit, smallestUnit: mode, roundingIncrement: n }).toString());
            attempt(() => Temporal.PlainTime.from(rel).round({ smallestUnit: unit, roundingMode: mode, roundingIncrement: n }).toString());
            return "ok";
        })()"#,
    );
}

// ---- regular expressions -------------------------------------------------------------------------

/// Regex host polls a search may take before it is stopped. A poll comes
/// every 65,536 matcher steps.
const REGEX_POLLS: u32 = 32;

/// The capture ranges of a match: `None` when the search was stopped,
/// `Some(None)` when nothing matched.
type Found = Option<Option<Vec<Option<(usize, usize)>>>>;

fn search(re: &Regex, input: &[u16], start: usize, sticky: bool) -> Found {
    let mut polls = 0u32;
    let mut poll = || {
        polls += 1;
        polls > REGEX_POLLS
    };
    let found = if sticky { re.exec_sticky_polled(input, start, &mut poll) } else { re.exec_polled(input, start, &mut poll) };
    found.ok().map(|m| m.map(|c| c.ranges))
}

/// Input lines: the flags, the pattern, then the subject. The pattern must
/// compile, or not, under every matcher mode alike; where a pattern can run
/// linearly the backtracker, the linear matcher and `Auto` must find the
/// same match and captures; and a compiled program must load back from its
/// bytes and match the same way.
pub fn regex(data: &[u8]) {
    let src = text(data);
    let mut lines = src.splitn(3, '\n');
    let flags: Vec<u16> = lines.next().unwrap_or("").encode_utf16().collect();
    let pattern: Vec<u16> = lines.next().unwrap_or("").encode_utf16().collect();
    let subject: Vec<u16> = lines.next().unwrap_or("").encode_utf16().collect();
    let Ok(flags) = Flags::parse(&flags) else { return };
    let _budget = StackBudget::enter(DEFAULT_BUDGET);
    let back = Regex::compile_with(&pattern, &flags, MatcherMode::BacktrackingOnly);
    let lin = Regex::compile_with(&pattern, &flags, MatcherMode::LinearWhenEligible);
    let auto = Regex::compile_with(&pattern, &flags, MatcherMode::Auto);
    let (back, lin, auto) = match (back, lin, auto) {
        (Ok(b), Ok(l), Ok(a)) => {
            accepted();
            (b, l, a)
        }
        (Err(_), Err(_), Err(_)) => return,
        (b, l, a) => panic!("the matcher modes disagree on whether the pattern compiles: {:?} {:?} {:?}", b.err(), l.err(), a.err()),
    };
    let reloaded = Regex::from_program_bytes(&back.program_bytes(), MatcherMode::BacktrackingOnly)
        .unwrap_or_else(|e| panic!("a compiled program does not load from its bytes: {e}"));
    let eligible = back.linear_eligibility().is_ok();
    let starts = [0, 1, subject.len() / 2, subject.len()];
    for &start in &starts {
        for sticky in [false, true] {
            let b = search(&back, &subject, start, sticky);
            let r = search(&reloaded, &subject, start, sticky);
            if let (Some(b), Some(r)) = (&b, &r) {
                assert_eq!(b, r, "a reloaded program matches differently (start {start}, sticky {sticky})");
            }
            let l = search(&lin, &subject, start, sticky);
            let a = search(&auto, &subject, start, sticky);
            if eligible {
                for (name, other) in [("linear", &l), ("auto", &a)] {
                    if let (Some(b), Some(o)) = (&b, other) {
                        assert_eq!(b, o, "backtracking and {name} disagree (start {start}, sticky {sticky})");
                    }
                }
            }
        }
    }
}

/// A compiled program, as bytecode carries it: the verifier must refuse it
/// or accept a program that matches without panicking.
pub fn regex_program(data: &[u8]) {
    let subjects: [&[u16]; 3] = [&[], &[0x61, 0x62, 0x63, 0x0A, 0x61], &[0xD83D, 0xDE00, 0x41, 0xDC00, 0x5F]];
    for mode in [MatcherMode::BacktrackingOnly, MatcherMode::Auto, MatcherMode::LinearWhenEligible] {
        let Ok(re) = Regex::from_program_bytes(data, mode) else { return };
        if mode == MatcherMode::Auto {
            accepted();
        }
        let _ = (re.group_count(), re.group_names().len(), re.heap_bytes(), re.starts_anchored());
        for s in subjects {
            for start in 0..=s.len() {
                let _ = search(&re, s, start, false);
                let _ = search(&re, s, start, true);
            }
        }
    }
}

// ---- bytecode and snapshots ----------------------------------------------------------------------

/// Containers made by this engine around a trivial script and module, and
/// where their code trees sit.
fn bytecode_bases() -> &'static [(Vec<u8>, std::ops::Range<usize>); 2] {
    static BASES: OnceLock<[(Vec<u8>, std::ops::Range<usize>); 2]> = OnceLock::new();
    BASES.get_or_init(|| {
        let make = |module: bool| {
            let code = if module {
                agnijs_js_compiler::compile_module("export {};", "base.mjs")
            } else {
                agnijs_js_compiler::compile_script("0;", "base.js")
            }
            .expect("the base compiles");
            let bytes = agnijs_js_vm::engine::encode_bytecode(module, &code);
            let span = agnijs_js_vm::engine::testing::tree_span(&bytes).expect("a tree span");
            (bytes, span)
        };
        [make(false), make(true)]
    })
}

/// A bytecode container holding `tree` as its code tree, resealed so that
/// the checksum passes: the first input byte picks a script (even) or
/// module (odd) container, the rest is the tree.
pub fn bytecode_container(data: &[u8]) -> Vec<u8> {
    let (select, tree) = data.split_first().map_or((0, data), |(s, t)| (*s, t));
    let (base, span) = &bytecode_bases()[usize::from(select & 1)];
    let mut out = Vec::with_capacity(base.len() + tree.len());
    out.extend_from_slice(&base[..span.start - 8]);
    out.extend_from_slice(&(tree.len() as u64).to_le_bytes());
    out.extend_from_slice(tree);
    out.extend_from_slice(&base[span.end..]);
    agnijs_js_vm::engine::testing::reseal(&mut out);
    out
}

/// The input is a code tree (see [`bytecode_container`]). The loader may
/// refuse it; whatever it accepts runs without a panic (ADR 0010, ADR 0022
/// TASK-103.30 addenda), and the engine stays usable.
pub fn bytecode(data: &[u8]) {
    let container = bytecode_container(data);
    let host = FuzzHost::new();
    let mut e = engine(&host);
    let realm = e.main_realm();
    if !matches!(e.eval_bytecode(realm, &container), Err(Error::BytecodeCorrupt(_) | Error::BytecodeMismatch { .. })) {
        accepted();
    }
    drain_jobs(&mut *e);
    assert_usable(&mut *e, &host);
}

/// The script the snapshot targets restore and probe.
pub const SNAPSHOT_SCRIPT: &str = "let n = 0; const inc = () => ++n;
class A { #p = 1; static s = [1, { t: 'x' }]; get p() { return this.#p; } }
var m = new Map([['k', new Set([1])]]), ta = new Int16Array(4), re = /a+/i;
var b = inc.bind(null); var sym = Symbol.for('s'); var dv = new DataView(new ArrayBuffer(8));
var w = new WeakMap([[A, 1]]); var p = Promise.resolve(5); function* g() { yield 1; }";

const SNAPSHOT_PROBE: &str = "try { inc(); new A().p; A.s[1].t; m.get('k').has(1); ta[1] = 3; re.test('AA'); b();
Symbol.keyFor(sym); dv.setInt8(0, 1); w.get(A); p.then(x => x); [...g()]; Object.keys(globalThis).length; } catch (e) {}";

fn snapshot_base() -> &'static (Vec<u8>, usize) {
    static BASE: OnceLock<(Vec<u8>, usize)> = OnceLock::new();
    BASE.get_or_init(|| {
        let mut e = agnijs_js_vm::make_engine(FuzzHost::new());
        let realm = e.main_realm();
        e.eval_script(realm, "base.js", SNAPSHOT_SCRIPT).expect("the base script runs");
        let bytes = e.snapshot(realm).expect("a snapshot");
        let body = 4 + agnijs_js_vm::snapshot::format_tag().len() + 1 + 16;
        (bytes, body)
    })
}

/// The body of a snapshot of [`SNAPSHOT_SCRIPT`]'s realm, which seeds the
/// snapshot target.
pub fn snapshot_body() -> Vec<u8> {
    let (bytes, body) = snapshot_base();
    bytes[*body..].to_vec()
}

/// A snapshot holding `body` behind the header of [`snapshot_body`]'s
/// snapshot, resealed so that the checksum passes.
pub fn snapshot_container(body: &[u8]) -> Vec<u8> {
    let (base, start) = snapshot_base();
    let mut bytes = Vec::with_capacity(start + body.len());
    bytes.extend_from_slice(&base[..*start]);
    bytes.extend_from_slice(body);
    agnijs_js_vm::snapshot::testing::reseal(&mut bytes);
    bytes
}

/// The input is a snapshot body behind a valid header, resealed. The
/// loader may refuse it; an engine it restores runs a probe without a panic
/// and stays usable.
pub fn snapshot(data: &[u8]) {
    let bytes = snapshot_container(data);
    let host = FuzzHost::new();
    let Ok(mut e) = agnijs_js_vm::make_engine_from_snapshot(host.clone(), &bytes) else { return };
    accepted();
    limit(&mut *e);
    let realm = e.main_realm();
    let _ = e.eval_script(realm, "probe.js", SNAPSHOT_PROBE);
    drain_jobs(&mut *e);
    assert_usable(&mut *e, &host);
}

// ---- the debugger protocol -----------------------------------------------------------------------

/// A line holding only this separates the messages handled before the
/// script runs from those delivered while it is paused.
pub const CDP_PAUSE_MARKER: &str = "--";

/// Messages from a fixed list; the session sees the transport close when
/// the list runs out or reaches [`CDP_PAUSE_MARKER`].
struct Script {
    incoming: RefCell<VecDeque<String>>,
    sent: Cell<usize>,
}

/// The session's end of a [`Script`].
struct Feed(Rc<Script>);

impl agnijs_js_cdp::Transport for Feed {
    fn send(&self, message: &str) {
        assert!(serde_json::from_str::<serde_json::Value>(message).is_ok(), "the session sent invalid JSON: {message}");
        self.0.sent.set(self.0.sent.get() + 1);
    }
    fn recv(&self, _wait: bool) -> agnijs_js_cdp::Incoming {
        match self.0.incoming.borrow_mut().pop_front() {
            Some(m) if m != CDP_PAUSE_MARKER => agnijs_js_cdp::Incoming::Message(m),
            _ => agnijs_js_cdp::Incoming::Closed,
        }
    }
}

/// The script the CDP target debugs: it pauses at `debugger`, throws and
/// catches, and logs.
const CDP_SCRIPT: &str = "function add(a, b) {
  const sum = a + b;
  console.log('sum', sum, { a, b });
  debugger;
  return sum;
}
try { throw new Error('caught'); } catch (e) {}
globalThis.total = add(2, 3);
Promise.reject(new TypeError('unhandled'));
";

/// One protocol message per input line. The lines before
/// [`CDP_PAUSE_MARKER`] are handled with the script not yet run; the
/// script then runs, and the lines after the marker are delivered while it
/// is paused. The session must answer with valid JSON and leave the engine
/// usable.
pub fn cdp(data: &[u8]) {
    let src = text(data);
    let script = Rc::new(Script {
        incoming: RefCell::new(src.lines().map(str::to_string).collect()),
        sent: Cell::new(0),
    });
    let host = FuzzHost::new();
    let mut d = agnijs_js_vm::make_debuggee(host.clone());
    limit(d.engine());
    let realm = d.engine().main_realm();
    let session = agnijs_js_cdp::Session::new(Feed(script.clone()), realm);
    session.attach(&mut *d);
    session.serve(&mut *d);
    let _ = d.engine().eval_script(realm, "app.js", CDP_SCRIPT);
    drain_jobs(d.engine());
    // Whatever is left arrives after the script; the session then sees
    // the transport close.
    while script.incoming.borrow().front().is_some() {
        let next = script.incoming.borrow_mut().pop_front();
        if let Some(m) = next.filter(|m| m != CDP_PAUSE_MARKER) {
            host.rearm();
            session.handle(&mut *d, &m);
        }
    }
    d.detach();
    assert_usable(d.engine(), &host);
}

/// WebSocket frames read one after another until the reader refuses one,
/// under a small message limit.
pub fn cdp_frames(data: &[u8]) {
    let mut r = std::io::Cursor::new(data);
    for _ in 0..64 {
        match agnijs_js_cdp::ws::read_frame_limited(&mut r, 1 << 16) {
            Ok(frame) => {
                accepted();
                assert!(frame.payload.len() <= 1 << 16, "a frame passed the limit");
            }
            Err(_) => break,
        }
    }
}

// ---- source maps and import maps -----------------------------------------------------------------

/// A Source Map v3 as JSON, also as a `data:` URL, and lookups across it.
pub fn source_map(data: &[u8]) {
    use agnijs_js_debugger::source_map::{base64_decode, percent_decode};
    use agnijs_js_debugger::SourceMap;
    let src = text(data);
    let lookups = |map: &SourceMap| {
        for line in [0, 1, 2, 7, 100, u32::MAX] {
            for column in [0, 1, 5, 40, u32::MAX] {
                let _ = map.lookup(line, column);
            }
        }
    };
    if let Ok(map) = SourceMap::parse(&src) {
        accepted();
        lookups(&map);
    }
    if let Some(Ok(map)) = SourceMap::from_data_url(&src) {
        lookups(&map);
    }
    let _ = base64_decode(&src);
    let _ = percent_decode(&src);
    let _ = agnijs_js_debugger::source_mapping_url(&src);
}

/// Input lines: the import map's JSON on the first line, then one
/// specifier per line, resolved from two referrers.
pub fn import_map(data: &[u8]) {
    use agnijs_js_resolve::ImportMap;
    let src = text(data);
    let mut lines = src.lines();
    let Ok(map) = ImportMap::parse(lines.next().unwrap_or(""), "file:///app/index.html") else { return };
    accepted();
    let _ = map.imports().count();
    for spec in lines {
        for base in ["file:///app/main.js", "https://example.com/a/b/c.js"] {
            if let Ok(url) = map.resolve(spec, base) {
                let _ = map.integrity(&url);
            }
        }
    }
}

// ---- TypeScript ----------------------------------------------------------------------------------

#[cfg(feature = "typescript")]
mod typescript;
#[cfg(feature = "typescript")]
pub use typescript::typescript;
