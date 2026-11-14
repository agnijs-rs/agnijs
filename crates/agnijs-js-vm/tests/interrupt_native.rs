//! The host interrupt reaches work done inside native code (TASK-103.28):
//! the regex matchers and the built-ins that loop over their input poll
//! it, so a script cannot hold the thread with one long native call.
//!
//! Each case arms a host whose interrupt hook always says stop, then runs
//! one call that would take seconds (or far longer) to finish. The call
//! must fail with `Error::Interrupted`, ask the hook, and return quickly.
//! The inputs are sized so that an engine that does not poll finishes
//! (with a result rather than an interrupt) in seconds, not hours.

use agnijs_engine_api::{Engine, Error, Host, RealmId, RegexMatcher, Value};
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

struct StopHost {
    matcher: RegexMatcher,
    stop: Cell<bool>,
    asked: Cell<u64>,
}

impl Host for StopHost {
    fn interrupt(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.stop.get()
    }
    fn regex_matcher(&self) -> RegexMatcher {
        self.matcher
    }
}

fn engine(matcher: RegexMatcher) -> (Rc<StopHost>, Box<dyn Engine>, RealmId) {
    let host = Rc::new(StopHost {
        matcher,
        stop: Cell::new(false),
        asked: Cell::new(0),
    });
    let e = agnijs_js_vm::make_engine(host.clone());
    let realm = e.main_realm();
    (host, e, realm)
}

/// How long an interrupted call may take, generous for debug builds on a
/// busy machine: the unpolled calls below take seconds to hours.
const BOUND: Duration = Duration::from_secs(2);

/// Run `setup` normally, then `hot` with the hook saying stop: it must be
/// interrupted within [`BOUND`], and the engine must carry on afterwards.
fn interrupted(matcher: RegexMatcher, setup: &str, hot: &str) {
    let (host, mut e, realm) = engine(matcher);
    e.eval_script(realm, "setup.js", setup)
        .unwrap_or_else(|err| panic!("{err}: {setup}"));
    host.stop.set(true);
    host.asked.set(0);
    let t = Instant::now();
    let r = e.eval_script(realm, "hot.js", hot);
    let took = t.elapsed();
    if !matches!(r, Err(Error::Interrupted)) {
        let got: String = format!("{r:?}").chars().take(80).collect();
        panic!("{hot}: expected an interrupt, got {got} after {took:?}");
    }
    assert!(host.asked.get() > 0, "{hot}: the hook was never asked");
    assert!(took < BOUND, "{hot}: the interrupt took {took:?}");
    host.stop.set(false);
    assert_eq!(
        e.eval_script(realm, "after.js", "1 + 1").unwrap(),
        Value::Number(2.0)
    );
}

// ---- regular expressions -------------------------------------------------------------------------

#[test]
fn an_exponential_lookahead_match_is_interrupted_under_auto() {
    // Lookaround keeps the pattern off the linear-time matcher, so it
    // backtracks with no step budget.
    interrupted(
        RegexMatcher::Auto,
        "globalThis.s = 'a'.repeat(25)",
        "/(?=(a+)+b)/.test(s)",
    );
}

#[test]
fn a_nested_quantifier_match_is_interrupted_under_backtracking_only() {
    interrupted(
        RegexMatcher::BacktrackingOnly,
        "globalThis.s = 'a'.repeat(25)",
        "/(a+)+b/.test(s)",
    );
}

#[test]
fn a_backreference_match_is_interrupted() {
    interrupted(
        RegexMatcher::Auto,
        "globalThis.s = 'a'.repeat(25)",
        "/(a+)+\\1b/.test(s)",
    );
}

#[test]
fn a_global_replace_with_millions_of_matches_is_interrupted() {
    interrupted(
        RegexMatcher::Auto,
        "globalThis.s = 'a'.repeat(3000000)",
        "s.replace(/a/g, 'b')",
    );
}

#[test]
fn a_global_split_and_match_are_interrupted() {
    interrupted(
        RegexMatcher::Auto,
        "globalThis.s = 'a,'.repeat(3000000)",
        "s.split(/,/)",
    );
    interrupted(
        RegexMatcher::Auto,
        "globalThis.s = 'a'.repeat(3000000)",
        "s.match(/a/g)",
    );
}

#[test]
fn interrupt_polling_does_not_change_what_a_pattern_matches() {
    // Patterns that take many polling slices to decide, run to completion
    // under each policy with a hook that never stops them.
    for matcher in [
        RegexMatcher::Auto,
        RegexMatcher::BacktrackingOnly,
        RegexMatcher::LinearWhenEligible,
    ] {
        let (host, mut e, realm) = engine(matcher);
        let r = e
            .eval_script(
                realm,
                "t.js",
                "const s = 'a'.repeat(18) + 'x';
                 [ /(?=(a+)+c)|x/.exec(s).index,
                   /(a+)+c|(x)/.exec(s)[2],
                   /(a|aa)+\\1c|a{3}x/.exec(s).index,
                   'ab'.repeat(5000).replace(/b/g, '').length ].join()",
            )
            .unwrap();
        assert_eq!(r, Value::String("18,x,15,5000".into()), "{matcher:?}");
        assert!(
            host.asked.get() > 0,
            "{matcher:?}: the hook was never asked"
        );
    }
}

// ---- native built-ins ----------------------------------------------------------------------------

#[test]
fn a_join_over_a_multi_million_element_array_is_interrupted() {
    interrupted(
        RegexMatcher::Auto,
        "globalThis.a = Array(3000000)",
        "a.join(',')",
    );
}

#[test]
fn array_methods_over_huge_array_likes_are_interrupted() {
    let setup = "globalThis.big = { length: 3000000 }; globalThis.arr = Array(3000000);";
    for hot in [
        "Array.prototype.indexOf.call(big, 1)",
        "Array.prototype.lastIndexOf.call(big, 1)",
        "Array.prototype.includes.call(big, 1)",
        "Array.prototype.fill.call(big, 1)",
        "Array.prototype.copyWithin.call(big, 0, 1)",
        "Array.prototype.forEach.call(big, () => {})",
        "Array.prototype.reverse.call(big)",
        "Array.prototype.join.call(big)",
        "Array.from(big)",
        "arr.sort()",
        "arr.slice()",
        "arr.toString()",
    ] {
        interrupted(RegexMatcher::Auto, setup, hot);
    }
}

#[test]
fn typed_array_methods_are_interrupted() {
    let setup = "globalThis.ta = new Uint8Array(20000000);";
    for hot in [
        "ta.join()",
        "ta.indexOf(1)",
        "ta.includes(1)",
        "ta.fill(1)",
        "Array.from(ta)",
    ] {
        interrupted(RegexMatcher::Auto, setup, hot);
    }
}

#[test]
fn a_quadratic_string_search_is_interrupted() {
    let setup =
        "globalThis.hay = 'a'.repeat(200000); globalThis.needle = 'a'.repeat(100000) + 'b';";
    for hot in [
        "hay.indexOf(needle)",
        "hay.includes(needle)",
        "hay.lastIndexOf(needle)",
        "hay.split(needle)",
    ] {
        interrupted(RegexMatcher::Auto, setup, hot);
    }
}

#[test]
fn json_over_a_huge_array_is_interrupted() {
    interrupted(
        RegexMatcher::Auto,
        "globalThis.a = Array(3000000).fill(0)",
        "JSON.stringify(a)",
    );
    interrupted(
        RegexMatcher::Auto,
        "globalThis.t = '[' + '0,'.repeat(3000000) + '0]'",
        "JSON.parse(t)",
    );
}

// ---- loops over native iterators (TASK-103.53) ---------------------------------------------------

/// Spread, the Promise combinators, `Intl.ListFormat` and the key lists of
/// a typed array run a native iterator or a bare length with no safe
/// point; they poll as they go.
#[test]
fn loops_over_native_iterators_are_interrupted() {
    let setup = "globalThis.ta = new Uint8Array(20000000);";
    for hot in [
        "[...new Array(1e8).keys()].length",
        "Promise.all(new Array(1e8).keys()); 0",
        "Promise.allSettled(new Array(1e8).keys()); 0",
        "Promise.any(new Array(1e8).keys()); 0",
        "Promise.race(new Array(1e8).keys()); 0",
        "Object.entries(ta).length",
        "new Intl.ListFormat('en').format(new Array(1e8).fill('a')).length",
    ] {
        interrupted(RegexMatcher::Auto, setup, hot);
    }
}

/// A spread collects inside its instruction whenever a collection is due,
/// and asks the host after each one: under AGNIJS_GC_STRESS that is every
/// element, and 4096 collections of a large live set between asks took
/// seconds. Without stress this is the case above.
#[test]
fn a_spread_that_collects_asks_the_host_after_each_collection() {
    let setup = "globalThis.keep = new Array(500000).fill(0);";
    interrupted(RegexMatcher::Auto, setup, "[...new Array(1e8).keys()].length");
}

// ---- BigInt arithmetic (TASK-103.55) -------------------------------------------------------------

/// Parsing, division, multiplication, exponentiation and conversion to a
/// string are superlinear in the digits; the large ones run in steps of
/// bounded work and poll between them.
#[test]
fn superlinear_big_int_work_is_interrupted() {
    let setup = "globalThis.a = (1n << 8000000n) - 1n; globalThis.b = (1n << 4000000n) - 3n;
                 globalThis.c = (1n << 100000000n) - 1n; globalThis.t = 1n << 2000000n;
                 globalThis.nines = '9'.repeat(1e6);";
    for hot in [
        "t.toString().length",
        "BigInt(nines); 0",
        "1n < nines",
        "1n == nines",
        "a / b; 0",
        "a % b; 0",
        "c * (c - 2n); 0",
        "3n ** 100000000n; 0",
        "String(t).length",
        "t.toString(7).length",
        "new Intl.NumberFormat('en').format(t).length",
    ] {
        interrupted(RegexMatcher::Auto, setup, hot);
    }
}

/// A result past 2^30 bits, the cap `<<` already applies, is refused
/// before any of it is computed.
#[test]
fn big_int_results_past_the_cap_are_refused_up_front() {
    let (_host, mut e, realm) = engine(RegexMatcher::Auto);
    for src in [
        "(1n << 1000000n) ** 100000n",
        "2n ** (2n ** 31n)",
        "(-3n) ** 10000000000n",
        "(1n << (1n << 29n)) * (1n << ((1n << 29n) + 2n))",
    ] {
        let t = Instant::now();
        let caught = format!(
            "try {{ {src}; 'no error' }} catch (e) {{ e instanceof RangeError ? e.message : String(e) }}"
        );
        let r = e.eval_script(realm, "t.js", &caught);
        assert_eq!(r.unwrap(), Value::string("Maximum BigInt size exceeded"), "{src}");
        assert!(t.elapsed() < BOUND, "{src}: took {:?}", t.elapsed());
    }
    // Trivial bases and the largest powers still work.
    for (src, want) in [
        ("1n ** (2n ** 64n)", "1"),
        ("(-1n) ** (2n ** 64n + 1n)", "-1"),
        ("0n ** (2n ** 64n)", "0"),
        ("2n ** 4096n === 1n << 4096n", "true"),
        ("(7n ** 100n) * (7n ** 50n) === 7n ** 150n", "true"),
    ] {
        let r = e.eval_script(realm, "t.js", &format!("String({src})"));
        assert_eq!(r.unwrap(), Value::string(want), "{src}");
    }
}

/// Power-of-two radixes are linear: a long hex string parses at once.
#[test]
fn power_of_two_radix_big_int_parsing_is_linear() {
    let (_host, mut e, realm) = engine(RegexMatcher::Auto);
    let t = Instant::now();
    let r = e.eval_script(
        realm,
        "t.js",
        "BigInt('0x' + 'f'.repeat(4e6)) === (1n << 16000000n) - 1n && BigInt('0b' + '1'.repeat(1e6)) === (1n << 1000000n) - 1n",
    );
    assert_eq!(r.unwrap(), Value::Bool(true));
    assert!(t.elapsed() < BOUND, "took {:?}", t.elapsed());
}

/// The polled paths give num-bigint's results: each operand is past the
/// size where the block algorithms take over, and each identity's other
/// side uses only shifts and additions, which are linear.
#[test]
fn polled_big_int_work_gives_exact_results() {
    let (_host, mut e, realm) = engine(RegexMatcher::Auto);
    let checks = [
        // Block multiplication: both operands over 65,536 digits.
        "const n = 2300000n, m = 2200000n;
         ((1n << n) - 1n) * ((1n << m) - 1n) === (1n << (n + m)) - (1n << n) - (1n << m) + 1n",
        "const n = 2300000n; -((1n << n) + 7n) * ((1n << n) - 5n) === -((1n << (2n * n)) + (2n << n) - 35n)",
        // Block division: 8,192 digits by 2,048.
        "const x = (1n << 262000n) + 123456789n * (1n << 100000n) + 987654321n, d = (1n << 65000n) - 12345n;
         const q = x / d, r = x % d;
         q * d + r === x && r >= 0n && r < d",
        "const x = (1n << 262000n) + 99n, d = (1n << 65000n) + 1n;
         (-x) / d === -(x / d) && (-x) % d === -(x % d) && x / (-d) === -(x / d) && x % (-d) === x % d",
        // Conversion to a string, in chunks of 19 digits, and parsing back.
        "(10n ** 50000n).toString() === '1' + '0'.repeat(50000)",
        "(10n ** 50000n - 1n).toString() === '9'.repeat(50000)",
        "String(-(10n ** 50000n) - 1n) === '-1' + '0'.repeat(49999) + '1'",
        "(7n ** 30000n).toString(7) === '1' + '0'.repeat(30000)",
        "(36n ** 20000n - 1n).toString(36) === 'z'.repeat(20000)",
        "BigInt('1' + '0'.repeat(50000)) === 10n ** 50000n",
        "BigInt('-' + '9'.repeat(50001)) === -(10n ** 50001n - 1n)",
        "const t = (1n << 300000n) / 3n; BigInt(t.toString()) === t && BigInt(String(-t)) === -t",
        "'1' + '0'.repeat(50000) == 10n ** 50000n && 10n ** 50000n > '9'.repeat(50000)",
        // Powers.
        "3n ** 2000000n === (3n ** 1000000n) * (3n ** 1000000n)",
        "(-2n) ** 3000001n === -(1n << 3000001n)",
    ];
    for src in checks {
        let r = e.eval_script(realm, "t.js", &format!("{{ {src} }}"));
        assert_eq!(r.unwrap(), Value::Bool(true), "{src}");
    }
}
