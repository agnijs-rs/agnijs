//! TypeScript for AgniJS embedders: TypeScript source in, JavaScript and a
//! Source Map v3 out, through [Oxc](https://oxc.rs) (ADR 0029).
//!
//! The engine never sees TypeScript and does not depend on this crate. A
//! host calls [`transpile`] where it loads code, typically in
//! `Host::load_module`, and hands the result to the engine with its map, so
//! that `Error.stack`, the host's `Exception::stack`, CallSite objects and
//! the debugger report original `.ts` positions (ADR 0024):
//!
//! ```
//! use agnijs_js_typescript::{transpile, Options};
//!
//! let ts = "enum Color { Red, Green }\nexport const c: Color = Color.Green;\n";
//! let out = transpile("color.ts", ts, &Options::default()).unwrap();
//! assert!(out.code.contains("Color[Color[\"Green\"] = 1] = \"Green\""));
//! assert!(out.source_map.contains("\"sources\":[\"color.ts\"]"));
//! // In a Host::load_module:
//! // Ok(ModuleSource::SourceWithMap { source: out.code, source_map: out.source_map })
//! ```
//!
//! # What is supported
//!
//! [`Mode::Transform`], the default, strips every type-level construct
//! (annotations, interfaces, type aliases, `declare`, type-only imports
//! and exports, `as`, `satisfies`, `!`, type arguments, `implements`,
//! overloads, `abstract` members, accessibility modifiers) and transforms
//! `enum` and `const enum` (emitted as plain enums, as `isolatedModules`
//! requires), namespaces, parameter properties, legacy decorators (with
//! [`Options::experimental_decorators`]) and JSX. [`Mode::StripOnly`]
//! accepts only erasable syntax, as Node's strip-only mode and TypeScript's
//! `--erasableSyntaxOnly` do. The syntax level is TypeScript 5.9 as parsed
//! by Oxc 0.158. Types are never checked.
//!
//! The output is ECMAScript for the engine, not for older runtimes: nothing
//! but TypeScript and JSX is lowered, and nothing is imported that the
//! source did not import. The crate refuses, with a position, what would
//! not run: TC39 decorators (only TypeScript's legacy decorators are
//! transformed), `accessor` fields, and the CommonJS forms
//! `import x = require()` and `export =`.
//!
//! # Limits
//!
//! Oxc's parser, analysis, transform and printer are recursive and have no
//! depth guard, so each call runs them on a worker thread whose stack is
//! sized from the input: 16 MiB plus 1 KiB per source byte (4 KiB in
//! debug builds). That is reserved address space, committed only as deep
//! as the input nests, and no input up to [`Options::max_source_len`]
//! (1 MiB by default) can overflow it. Pathological nesting can touch the
//! whole reservation, so the limit is also the memory bound: lower it for
//! untrusted input. The printer's indentation, which grows with the square
//! of the nesting, is dropped for input nested far deeper than real code,
//! so the output stays proportional to the input. Time is linear in the
//! source for every pattern tried but nested `switch` statements, which
//! Oxc's analysis handles in quadratic time: about 2.3 s for the worst
//! 1 MiB in a release build. Transpiling happens before the engine runs anything, so
//! fuel, interrupts and the engine's memory limit do not apply to it. On
//! targets without threads (`wasm32-unknown-unknown`) the pipeline runs on
//! the caller's stack and is safe only for trusted input.
//!
//! # Determinism
//!
//! The output and the map depend only on the source, the file name and the
//! options: the same bytes on every run and every platform.

#![forbid(unsafe_code)]

mod check;
mod helpers;
mod pipeline;
mod position;

use std::fmt;

/// How much TypeScript is accepted. See the [crate documentation](crate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    /// Strip the types and transform the syntax with run-time meaning:
    /// `enum`, namespaces, parameter properties, legacy decorators.
    #[default]
    Transform,
    /// Accept erasable syntax only. Every enum, namespace with values,
    /// parameter property, decorator and `import x = y` alias is an error.
    StripOnly,
}

/// Whether the code will run as a module (`Engine::load_module`) or as a
/// script (`Engine::eval_script`). It decides the grammar (top-level
/// `await`, `import`, strictness) and must match how the output is run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SourceKind {
    #[default]
    Module,
    Script,
}

/// How JSX is compiled, as TypeScript's `jsx` option.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsxRuntime {
    /// Calls to [`Options::jsx_factory`] and [`Options::jsx_fragment_factory`]
    /// (`"react"` in `tsconfig.json`). Adds no import.
    #[default]
    Classic,
    /// Calls to the functions of `<jsx_import_source>/jsx-runtime`, imported
    /// by the output (`"react-jsx"`). Modules only.
    Automatic,
}

/// Transpile options. Start from [`Options::default`] or [`Options::for_path`]
/// and set fields.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Options {
    /// Default [`Mode::Transform`].
    pub mode: Mode,
    /// Default [`SourceKind::Module`].
    pub kind: SourceKind,
    /// Parse JSX (`.tsx`). Default `false`.
    pub jsx: bool,
    /// Default [`JsxRuntime::Classic`].
    pub jsx_runtime: JsxRuntime,
    /// The classic runtime's element function. Default `React.createElement`.
    pub jsx_factory: String,
    /// The classic runtime's fragment. Default `React.Fragment`.
    pub jsx_fragment_factory: String,
    /// The automatic runtime's package. Default `react`.
    pub jsx_import_source: String,
    /// Transform TypeScript's legacy decorators (`experimentalDecorators`).
    /// Off by default, as in TypeScript; decorators are then refused,
    /// because the standard decorators they would mean are not implemented.
    pub experimental_decorators: bool,
    /// Emit `design:type`, `design:paramtypes` and `design:returntype`
    /// metadata for legacy decorators (`emitDecoratorMetadata`). It calls
    /// `Reflect.metadata` when a polyfill provides it. Default `false`.
    pub emit_decorator_metadata: bool,
    /// Keep imports whose bindings are never used as values, as
    /// TypeScript's `verbatimModuleSyntax` does; only `import type` and
    /// `type` specifiers are removed. Default `false`: such imports are
    /// removed, as `tsc` removes them.
    pub verbatim_module_syntax: bool,
    /// The largest source accepted, in bytes. Default 1 MiB. It bounds the
    /// worker thread's stack reservation (see the [crate documentation](crate)).
    pub max_source_len: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            mode: Mode::Transform,
            kind: SourceKind::Module,
            jsx: false,
            jsx_runtime: JsxRuntime::Classic,
            jsx_factory: "React.createElement".into(),
            jsx_fragment_factory: "React.Fragment".into(),
            jsx_import_source: "react".into(),
            experimental_decorators: false,
            emit_decorator_metadata: false,
            verbatim_module_syntax: false,
            max_source_len: 1 << 20,
        }
    }
}

impl Options {
    /// The defaults for a file, by extension: `.ts` and `.mts` are modules,
    /// `.cts` a script, `.tsx` a module with JSX. `None` for any other
    /// extension, `.d.ts` files included (they hold no code).
    pub fn for_path(path: &str) -> Option<Options> {
        let lower = path.to_ascii_lowercase();
        if [".d.ts", ".d.mts", ".d.cts"]
            .iter()
            .any(|e| lower.ends_with(e))
        {
            return None;
        }
        let mut o = Options::default();
        if lower.ends_with(".tsx") {
            o.jsx = true;
        } else if lower.ends_with(".cts") {
            o.kind = SourceKind::Script;
        } else if !(lower.ends_with(".ts") || lower.ends_with(".mts")) {
            return None;
        }
        Some(o)
    }
}

/// The JavaScript for a TypeScript source.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Output {
    /// ECMAScript the engine runs, as a module or a script per
    /// [`Options::kind`].
    pub code: String,
    /// A Source Map v3 from `code` to the TypeScript source, as JSON. Its
    /// `sources` is the file name given to [`transpile`], and it carries
    /// the source text in `sourcesContent`, so a debugger can show it.
    pub source_map: String,
}

/// A syntax error, or a construct the crate refuses, in TypeScript source.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Diagnostic {
    /// The file name given to [`transpile`].
    pub file: String,
    pub message: String,
    /// A hint on how to fix it, when there is one.
    pub help: Option<String>,
    /// The byte range in the source.
    pub start: u32,
    pub end: u32,
    /// The 1-based line of `start`.
    pub line: u32,
    /// The 1-based column of `start`, in UTF-16 units, as the engine counts
    /// columns.
    pub column: u32,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.file, self.line, self.column, self.message
        )?;
        if let Some(help) = &self.help {
            write!(f, "\n  help: {help}")?;
        }
        Ok(())
    }
}

/// The most diagnostics an [`Error::Syntax`] carries; the first ones by
/// position are kept.
pub const MAX_DIAGNOSTICS: usize = 100;

/// Why [`transpile`] failed.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Syntax errors, early errors and refused constructs, in source order:
    /// the first [`MAX_DIAGNOSTICS`] of them. Never empty.
    Syntax(Vec<Diagnostic>),
    /// The source is longer than [`Options::max_source_len`].
    TooLarge { len: usize, max: usize },
    /// The options contradict each other (for example the automatic JSX
    /// runtime, which imports, in a script).
    InvalidOptions(String),
    /// The worker thread could not be started, usually because its stack
    /// could not be reserved.
    Resource(String),
    /// Oxc panicked: a bug in it, reported instead of unwound.
    Internal(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Syntax(diags) => {
                for (i, d) in diags.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{d}")?;
                }
                Ok(())
            }
            Error::TooLarge { len, max } => {
                write!(
                    f,
                    "TypeScript source of {len} bytes is over the limit of {max}"
                )
            }
            Error::InvalidOptions(m) => write!(f, "invalid TypeScript options: {m}"),
            Error::Resource(m) => write!(f, "could not start the TypeScript worker: {m}"),
            Error::Internal(m) => write!(f, "internal error in the TypeScript transpiler: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// The worker stack for `len` bytes of source: 16 MiB plus 1 KiB per byte
/// in release, 4 KiB per byte with debug assertions (ADR 0029 has the
/// measurements; the deepest pattern costs 451 bytes per source byte in
/// release and 1,704 in debug).
pub fn worker_stack_size(len: usize) -> usize {
    let per_byte: usize = if cfg!(debug_assertions) { 4096 } else { 1024 };
    (16usize << 20).saturating_add(len.saturating_mul(per_byte))
}

/// Turns TypeScript into JavaScript plus a source map. `file` names the
/// source in the map and in diagnostics; use the name the engine will run
/// the code under.
pub fn transpile(file: &str, source: &str, options: &Options) -> Result<Output, Error> {
    if source.len() > options.max_source_len {
        return Err(Error::TooLarge {
            len: source.len(),
            max: options.max_source_len,
        });
    }
    if options.jsx
        && options.jsx_runtime == JsxRuntime::Automatic
        && options.kind == SourceKind::Script
    {
        return Err(Error::InvalidOptions(
            "the automatic JSX runtime imports its functions, so it needs a module".into(),
        ));
    }
    run_on_worker(source.len(), || pipeline::run(file, source, options))
}

#[cfg(not(target_family = "wasm"))]
fn run_on_worker(
    len: usize,
    job: impl FnOnce() -> Result<Output, Error> + Send,
) -> Result<Output, Error> {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .name("agnijs-typescript".into())
            .stack_size(worker_stack_size(len))
            .spawn_scoped(scope, job)
            .map_err(|e| Error::Resource(e.to_string()))?;
        worker
            .join()
            .unwrap_or_else(|payload| Err(Error::Internal(panic_message(&*payload))))
    })
}

#[cfg(target_family = "wasm")]
fn run_on_worker(
    _len: usize,
    job: impl FnOnce() -> Result<Output, Error>,
) -> Result<Output, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
        .unwrap_or_else(|payload| Err(Error::Internal(panic_message(&*payload))))
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".into()
    }
}
