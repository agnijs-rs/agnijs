//! Dropping the syntax tree of a left-associative chain of a million links
//! (TASK-103.46). The parser builds these chains in a loop, so the tree is a
//! million levels deep; dropping it must not recurse once per level.
//!
//! A stack overflow ends the process, so each chain is parsed and dropped
//! in a child copy of this test binary, on a thread with the default stack.

use std::process::Command;

const CHILD: &str = "AGNIJS_PARSER_DEEP_CHAIN_CASE";
const LINKS: usize = 1_000_000;

/// (name, head, link): the source is `head` then `link` a million times.
const CHAINS: &[(&str, &str, &str)] = &[
    ("binary", "1", "+1"),
    ("mixed binary", "1", "*1-1"),
    ("relational", "a", " in a"),
    ("logical or", "0", "||0"),
    ("logical and", "1", "&&1"),
    ("nullish", "0", "??0"),
    ("member", "a", ".p"),
    ("computed member", "a", "[0]"),
    ("optional chain", "a", "?.p"),
    ("optional call", "a", "?.()"),
    ("call", "a", "()"),
    ("new", "new a", ".p"),
    ("tagged template", "a", "``"),
    ("mixed member and call", "a", ".p()[0]``"),
];

/// The child side: parses and drops the chain named in the environment.
/// Does nothing in a normal test run.
#[test]
fn child() {
    let Ok(name) = std::env::var(CHILD) else {
        return;
    };
    let &(_, head, link) = CHAINS.iter().find(|c| c.0 == name).expect("unknown chain");
    std::thread::spawn(move || {
        let src = format!("{head}{}", link.repeat(LINKS));
        // Parsed, then dropped.
        let script = agnijs_js_parser::parse_script(&src);
        assert!(script.is_ok(), "{name}: {:?}", script.err());
        drop(script);
        // A chain the parser gives up on at its end is dropped too.
        let broken = agnijs_js_parser::parse_script(&format!("{src} +"));
        assert!(broken.is_err(), "{name}");
        // In a module and assigned to a binding.
        let module = agnijs_js_parser::parse_module(&format!("export let x = {src};"));
        assert!(module.is_ok(), "{name}: {:?}", module.err());
    })
    .join()
    .expect("the case panicked");
}

#[test]
fn chains_of_a_million_links_drop_without_recursing() {
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for (name, _, _) in CHAINS {
        let out = Command::new(&exe)
            .args(["--exact", "child", "--nocapture", "--test-threads=1"])
            .env(CHILD, name)
            .output()
            .unwrap();
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let tail: String = stderr.lines().rev().take(4).collect::<Vec<_>>().join(" | ");
            failed.push(format!("{name}: {:?} {tail}", out.status));
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
