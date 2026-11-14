//! Left-associative chains of a million links (TASK-103.46): binary and
//! logical operators, `??`, member, optional-chain, call, `new` and tagged
//! template chains. The parser builds them in a loop, so the syntax tree is
//! a million levels deep; the compiler refuses it with a catchable error,
//! and dropping the tree must not recurse once per level.
//!
//! A stack overflow ends the process, so each case runs in a child copy of
//! this test binary on a thread with the default stack: a regression fails
//! one test instead of aborting the suite.

use agnijs_engine_api::conformance::TestHost;
use agnijs_engine_api::{Engine, Error, Value};
use agnijs_js_vm::Vm;
use std::process::Command;

const CHILD: &str = "AGNIJS_DEEP_CHAIN_CASE";
const LINKS: usize = 1_000_000;

/// (name, head, link): the source is `head` then `link` a million times.
const CHAINS: &[(&str, &str, &str)] = &[
    ("binary", "1", "+1"),
    ("mixed binary", "1", "*1-1"),
    ("logical or", "0", "||0"),
    ("logical and", "1", "&&1"),
    ("nullish", "0", "??0"),
    ("member", "Object", ".p"),
    ("computed member", "Object", "[0]"),
    ("optional chain", "Object", "?.p"),
    ("call", "Object", "()"),
    ("new", "new Object", ".p"),
    ("tagged template", "String.raw", "``"),
];

fn run(script: &str) -> Result<Value, Error> {
    let mut vm = Vm::new(TestHost::new());
    vm.heap.stress = false;
    let realm = vm.main_realm();
    vm.eval_script(realm, "chain.js", script)
}

/// At top level: the script itself is the chain.
fn top_level(head: &str, link: &str) -> Result<(), String> {
    let src = format!("{head}{}", link.repeat(LINKS));
    match run(&src) {
        Err(Error::Exception(x)) if x.name == "SyntaxError" || x.name == "RangeError" => Ok(()),
        other => Err(format!("{other:?}")),
    }
}

/// Inside `eval`, where script can catch the error.
fn in_eval(head: &str, link: &str) -> Result<(), String> {
    let src = format!(
        "try {{ eval({head:?} + {link:?}.repeat({LINKS})); 'no error' }} catch (e) {{ e.name }}"
    );
    match run(&src) {
        Ok(Value::String(s)) if s.to_string() == "SyntaxError" || s.to_string() == "RangeError" => {
            Ok(())
        }
        other => Err(format!("{other:?}")),
    }
}

/// The child side: runs the case named in the environment
/// (`<chain>/top` or `<chain>/eval`). Does nothing in a normal test run.
#[test]
fn child() {
    let Ok(case) = std::env::var(CHILD) else {
        return;
    };
    let (name, mode) = case.rsplit_once('/').unwrap();
    let &(_, head, link) = CHAINS.iter().find(|c| c.0 == name).expect("unknown chain");
    let mode = mode.to_string();
    let r = std::thread::spawn(move || {
        if mode == "top" {
            top_level(head, link)
        } else {
            in_eval(head, link)
        }
    })
    .join()
    .expect("the case panicked");
    if let Err(e) = r {
        panic!("{case}: {e}");
    }
}

#[test]
fn chains_of_a_million_links_are_catchable_errors() {
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for (name, _, _) in CHAINS {
        for mode in ["top", "eval"] {
            let case = format!("{name}/{mode}");
            let out = Command::new(&exe)
                .args(["--exact", "child", "--nocapture", "--test-threads=1"])
                .env(CHILD, &case)
                .output()
                .unwrap();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
                failed.push(format!("{case}: {:?} {tail}", out.status));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
