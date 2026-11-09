//! RegExp exec against strings the VM converts to UTF-16 for the matcher:
//! results must not depend on what ran before, and a long input must not be
//! converted again on every exec just because a short string was matched in
//! between (a tokenizer does exactly that, see TASK-100.10).

use gable_engine_api::conformance::TestHost;
use gable_engine_api::{Engine, Value};
use gable_js_vm::Vm;
use std::time::Instant;

fn run(stress: bool, src: &str) -> Value {
    let mut vm = Vm::new(TestHost::new());
    Vm::create_realm(&mut vm);
    vm.heap.stress = stress;
    let realm = vm.main_realm();
    let v = vm
        .eval_script(realm, "regexp-test.js", src)
        .unwrap_or_else(|e| panic!("{e}"));
    vm.run_jobs(realm).unwrap();
    v
}

#[test]
fn alternating_inputs_match_against_their_own_text() {
    let src = "var log = [];
         var long = 'ab '.repeat(300) + 'Zed', words = ['x', 'Y', 'ab', 'Q'];
         var id = /[A-Za-z]+/y, up = /^[A-Z]/;
         for (var i = 0; i < long.length; i += 3) {
           id.lastIndex = i;
           var m = id.exec(long);
           log.push(m ? m[0] : '-', up.test(words[i % 4]) ? 'U' : 'l');
         }
         var s = ''; for (var k = 0; k < 4; k++) s += 'k' + k;
         log.push(/k(\\d)$/.exec(s)[1], 'xyz'.replace(/y/g, s.slice(0, 2)));
         log.slice(0, 8).join() + '|' + log.slice(-6).join()";
    for stress in [false, true] {
        assert_eq!(run(stress, src), Value::string("ab,l,ab,U,ab,l,ab,U|ab,U,Zed,l,3,xk0z"), "stress={stress}");
    }
}

fn tokenizer_ms(len: usize) -> f64 {
    // Probe a long line with a sticky regex, testing each short word with a
    // second regex in between, as packages/ui's highlighter does.
    let src = format!(
        "var s = 'a '.repeat({half}), id = /[a-z]+/y, n = 0;
         for (var i = 0; i < 40000; i += 2) {{
           id.lastIndex = i % s.length;
           var m = id.exec(s);
           if (/^[A-Z]/.test(m[0])) n++;
         }}
         n",
        half = len / 2
    );
    let t = Instant::now();
    assert_eq!(run(false, &src), Value::Number(0.0));
    t.elapsed().as_secs_f64() * 1000.0
}

#[test]
fn exec_on_a_long_string_does_not_reconvert_it_after_a_short_one() {
    // Same work on a 4k and on a 500k code-unit line: re-converting the long
    // line per exec made the second ~30x slower; now they cost the same.
    let short = tokenizer_ms(4_000);
    let long = tokenizer_ms(500_000);
    assert!(long < short * 4.0 + 200.0, "4k line {short:.0} ms, 500k line {long:.0} ms");
}

/// A host that opts its engine out of the linear-time matcher.
struct BacktrackingHost;

impl gable_engine_api::Host for BacktrackingHost {
    fn regex_matcher(&self) -> gable_engine_api::RegexMatcher {
        gable_engine_api::RegexMatcher::BacktrackingOnly
    }
}

#[test]
fn the_host_picks_the_matcher_per_engine_at_creation() {
    // ADR 0013: 14.9 s on the backtracking matcher with 28 a's.
    #[cfg(feature = "linear-regex")]
    {
        let t = Instant::now();
        assert_eq!(run(false, "/(a+)+$/.test('a'.repeat(40) + '!')"), Value::Bool(false));
        assert!(t.elapsed().as_secs() < 2, "auto took {:?}", t.elapsed());
    }

    // An opted-out engine backtracks; one beside it keeps the default.
    let mut out = Vm::new(std::rc::Rc::new(BacktrackingHost));
    Vm::create_realm(&mut out);
    assert_eq!(out.regex_mode, gable_js_regex::MatcherMode::BacktrackingOnly);
    let realm = out.main_realm();
    let ordinary = "/(\\d+)-(\\d+)/.exec('x 12-345 y').slice(1).join()";
    assert_eq!(out.eval_script(realm, "a.js", ordinary).unwrap(), Value::string("12,345"));
    let mut auto = Vm::new(TestHost::new());
    Vm::create_realm(&mut auto);
    assert_eq!(auto.regex_mode, gable_js_regex::MatcherMode::Auto);
}
