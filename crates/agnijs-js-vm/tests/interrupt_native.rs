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
