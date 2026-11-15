//! Fuzz targets for AgniJS (TASK-103.18, `docs/fuzzing.md`).
//!
//! Each target is a function of one byte string. It returns normally for
//! every input the component may legitimately refuse or fail on, and panics
//! when an input breaks one of the component's promises: a panic inside the
//! engine, an engine left poisoned or unusable, our own output that does not
//! load back, or two matchers that disagree.
//!
//! The same functions are run two ways:
//!
//! - by the `agnijs-fuzz` binary in this crate, a seeded mutating runner
//!   with a bounded default that CI runs on every push;
//! - by the cargo-fuzz targets in `fuzz/`, which hand them to libFuzzer
//!   for open-ended, coverage-guided runs.

#![forbid(unsafe_code)]

pub mod mutate;
pub mod seeds;
mod targets;

pub use targets::*;

/// How a target's inputs are mutated by the seeded runner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// JavaScript or other text: token, line and character edits.
    Text,
    /// Binary containers: byte and field edits.
    Binary,
}

/// One fuzz target.
pub struct Target {
    /// The name used on the command line and by `fuzz/fuzz_targets`.
    pub name: &'static str,
    /// What it exercises, for `--list`.
    pub about: &'static str,
    pub shape: Shape,
    /// The function under test.
    pub run: fn(&[u8]),
    /// The inputs mutation starts from.
    pub seeds: fn() -> Vec<Vec<u8>>,
    /// Iterations of the bounded default run.
    pub default_iters: usize,
}

/// Every target, in the order `all` runs them.
pub const TARGETS: &[Target] = &[
    Target {
        name: "parse",
        about: "the parser, as a script and as a module, under the native stack budget",
        shape: Shape::Text,
        run: parse,
        seeds: seeds::js,
        default_iters: 50_000,
    },
    Target {
        name: "compile",
        about: "parse and compile; the code tree and the bytecode container must decode back",
        shape: Shape::Text,
        run: compile,
        seeds: seeds::js,
        default_iters: 30_000,
    },
    Target {
        name: "eval",
        about: "the engine interface: run a script or module under fuel, an interrupt and a memory limit",
        shape: Shape::Text,
        run: eval,
        seeds: seeds::js,
        default_iters: 5_000,
    },
    Target {
        name: "json",
        about: "JSON.parse and JSON.stringify; stringify(parse(x)) must be a fixed point",
        shape: Shape::Text,
        run: json,
        seeds: seeds::json,
        default_iters: 5_000,
    },
    Target {
        name: "intl",
        about: "the Intl constructors and the locale-sensitive methods with fuzzed locales and options",
        shape: Shape::Text,
        run: intl,
        seeds: seeds::intl,
        default_iters: 2_000,
    },
    Target {
        name: "regex",
        about: "regex syntax and matching; the backtracker and the linear matcher must agree",
        shape: Shape::Text,
        run: regex,
        seeds: seeds::regex,
        default_iters: 20_000,
    },
    Target {
        name: "regex-program",
        about: "the compiled regex program verifier that loads programs from bytecode",
        shape: Shape::Binary,
        run: regex_program,
        seeds: seeds::regex_programs,
        default_iters: 50_000,
    },
    Target {
        name: "bytecode",
        about: "the bytecode loader, past the checksum, then running what loads",
        shape: Shape::Binary,
        run: bytecode,
        seeds: seeds::bytecode_trees,
        default_iters: 5_000,
    },
    Target {
        name: "snapshot",
        about: "the snapshot loader, past the checksum, then a probe script",
        shape: Shape::Binary,
        run: snapshot,
        seeds: seeds::snapshot_bodies,
        default_iters: 3_000,
    },
    Target {
        name: "cdp",
        about: "DevTools protocol messages against a debuggee, before and during a pause",
        shape: Shape::Text,
        run: cdp,
        seeds: seeds::cdp,
        default_iters: 3_000,
    },
    Target {
        name: "cdp-frames",
        about: "the WebSocket frame reader of the CDP server",
        shape: Shape::Binary,
        run: cdp_frames,
        seeds: seeds::ws_frames,
        default_iters: 50_000,
    },
    Target {
        name: "source-map",
        about: "Source Map v3 and data: URL parsing and lookups",
        shape: Shape::Text,
        run: source_map,
        seeds: seeds::source_maps,
        default_iters: 50_000,
    },
    Target {
        name: "import-map",
        about: "import map parsing and specifier resolution",
        shape: Shape::Text,
        run: import_map,
        seeds: seeds::import_maps,
        default_iters: 30_000,
    },
];

/// The target called `name`.
pub fn target(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}
