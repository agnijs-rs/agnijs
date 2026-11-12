//! `agnijs`: run a file, evaluate code or start a REPL on the AgniJS
//! engine. See `crates/agnijs-cli/README.md` for usage.

mod host;
mod inspect;
mod repl;
mod runtime;

use agnijs::api::{self, Engine, ModuleSource, RegexMatcher};
use agnijs::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use host::{absolute, CliHost, HostConfig};
use runtime::{exit, Failure, Limits, Runtime, Shown};
use std::io::{IsTerminal, Read};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const AFTER_HELP: &str = "\
With no command, `agnijs FILE` runs FILE and `agnijs` alone starts the REPL
(or runs standard input when it is not a terminal).

Exit status: 0 success, 1 uncaught exception or unhandled rejection,
2 usage or file error, 3 fuel or memory limit reached, 13 top-level await
never settled, 130 interrupted (Ctrl-C).";

#[derive(Parser)]
#[command(
    name = "agnijs",
    version,
    about = "Run JavaScript on the AgniJS engine",
    after_help = AFTER_HELP,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Run a script or module file (`-` reads standard input)
    Run(RunArgs),
    /// Evaluate code given on the command line
    Eval(EvalArgs),
    /// Start the interactive REPL
    Repl(ReplArgs),
    /// Precompile a file to bytecode that `agnijs run` loads
    Compile(CompileArgs),
    /// Run bootstrap scripts and save the realm as a snapshot for --snapshot
    Snapshot(SnapshotArgs),
}

#[derive(Args, Clone, Default)]
struct EngineOpts {
    /// Stop after this many bytecode instructions (deterministic; exit 3)
    #[arg(long, value_name = "N")]
    fuel: Option<u64>,
    /// Cap the heap, in bytes (suffixes K, M, G)
    #[arg(long, value_name = "BYTES", value_parser = parse_size)]
    max_memory: Option<usize>,
    /// Regular-expression matcher policy
    #[arg(long, value_enum, default_value_t = Matcher::Auto)]
    regex_matcher: Matcher,
    /// Seed Math.random, for reproducible runs
    #[arg(long, value_name = "N")]
    seed: Option<u64>,
    /// Start from a realm snapshot written by `agnijs snapshot`
    #[arg(long, value_name = "FILE", conflicts_with_all = ["inspect", "inspect_brk"])]
    snapshot: Option<String>,
    /// Serve the Chrome DevTools Protocol (default 127.0.0.1:9229)
    #[arg(long, value_name = "[HOST:]PORT", num_args = 0..=1, require_equals = true,
          default_missing_value = "127.0.0.1:9229")]
    inspect: Option<String>,
    /// Like --inspect, and wait for a client, then pause before the first statement
    #[arg(long, value_name = "[HOST:]PORT", num_args = 0..=1, require_equals = true,
          default_missing_value = "127.0.0.1:9229", conflicts_with = "inspect")]
    inspect_brk: Option<String>,
}

#[derive(Args, Clone, Default)]
struct ModeOpts {
    /// Treat the code as an ES module
    #[arg(long, short = 'm', conflicts_with = "script")]
    module: bool,
    /// Treat the code as a classic script
    #[arg(long)]
    script: bool,
}

#[derive(Args, Clone, Default)]
struct RunArgs {
    #[command(flatten)]
    engine: EngineOpts,
    #[command(flatten)]
    mode: ModeOpts,
    /// The file to run; `-` for standard input
    file: Option<String>,
    /// Arguments for the program, in the global `scriptArgs` after the file name
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct EvalArgs {
    #[command(flatten)]
    engine: EngineOpts,
    #[command(flatten)]
    mode: ModeOpts,
    /// Print the completion value (scripts only)
    #[arg(long, short = 'p')]
    print: bool,
    /// The code
    code: String,
    /// Arguments for the program, in the global `scriptArgs`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Args)]
struct ReplArgs {
    #[command(flatten)]
    engine: EngineOpts,
}

#[derive(Args)]
struct CompileArgs {
    #[command(flatten)]
    mode: ModeOpts,
    /// The source file
    file: String,
    /// Where to write the bytecode (default: the file with extension .jsc)
    #[arg(long, short = 'o', value_name = "FILE")]
    output: Option<String>,
}

#[derive(Args)]
struct SnapshotArgs {
    /// Bootstrap scripts, run in order (no modules, timers or host objects)
    scripts: Vec<String>,
    /// Where to write the snapshot
    #[arg(long, short = 'o', value_name = "FILE")]
    output: String,
    /// Regular-expression matcher policy while the bootstrap runs
    #[arg(long, value_enum, default_value_t = Matcher::Auto)]
    regex_matcher: Matcher,
}

#[derive(Clone, Copy, Default, ValueEnum)]
enum Matcher {
    #[default]
    Auto,
    BacktrackingOnly,
    LinearWhenEligible,
}

impl From<Matcher> for RegexMatcher {
    fn from(m: Matcher) -> RegexMatcher {
        match m {
            Matcher::Auto => RegexMatcher::Auto,
            Matcher::BacktrackingOnly => RegexMatcher::BacktrackingOnly,
            Matcher::LinearWhenEligible => RegexMatcher::LinearWhenEligible,
        }
    }
}

fn parse_size(s: &str) -> Result<usize, String> {
    let s = s.trim();
    let (num, mul) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => {
            let mul: usize = match c.to_ascii_uppercase() {
                'K' => 1 << 10,
                'M' => 1 << 20,
                'G' => 1 << 30,
                _ => return Err(format!("unknown size suffix '{c}' (use K, M or G)")),
            };
            (&s[..i], mul)
        }
        _ => (s, 1),
    };
    let n: usize = num
        .parse()
        .map_err(|_| format!("'{s}' is not a size in bytes"))?;
    n.checked_mul(mul).ok_or_else(|| "size too large".into())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Some(Command::Run(a)) => cmd_run(a),
        Some(Command::Eval(a)) => cmd_eval(a),
        Some(Command::Repl(a)) => cmd_repl(a.engine),
        Some(Command::Compile(a)) => cmd_compile(a),
        Some(Command::Snapshot(a)) => cmd_snapshot(a),
        None if cli.run.file.is_some() => cmd_run(cli.run),
        None if std::io::stdin().is_terminal() => cmd_repl(cli.run.engine),
        None => cmd_run(RunArgs {
            file: Some("-".into()),
            ..cli.run
        }),
    };
    ExitCode::from(code.clamp(0, 255) as u8)
}

// ---- building an engine ------------------------------------------------

/// The Ctrl-C flag: set by the handler, polled by the engine and the
/// event loop. A second Ctrl-C while the first is unanswered exits.
fn interrupt_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = flag.clone();
    let _ = ctrlc::set_handler(move || {
        if f.swap(true, Ordering::SeqCst) {
            std::process::exit(exit::INTERRUPTED);
        }
    });
    flag
}

fn make_host(opts: &EngineOpts) -> Rc<CliHost> {
    Rc::new(CliHost::new(HostConfig {
        regex_matcher: opts.regex_matcher.into(),
        seed: opts.seed,
        interrupt: interrupt_flag(),
    }))
}

/// `--inspect=9230` means port 9230 on the loopback interface.
fn inspect_addr(a: &str) -> String {
    if a.chars().all(|c| c.is_ascii_digit()) {
        format!("127.0.0.1:{a}")
    } else {
        a.to_string()
    }
}

fn make_runtime(opts: &EngineOpts, title: &str) -> Result<Runtime, Failure> {
    let host = make_host(opts);
    let ctx = if let Some(path) = &opts.snapshot {
        let bytes = std::fs::read(path).map_err(|e| {
            eprintln!("agnijs: cannot read {path}: {e}");
            Failure(exit::USAGE)
        })?;
        Context::from_snapshot(host.clone(), &bytes).map_err(|e| {
            eprintln!("agnijs: {path} is not a usable snapshot: {e}");
            Failure(exit::USAGE)
        })?
    } else if let Some((addr, wait)) = opts
        .inspect
        .as_deref()
        .map(|a| (a, false))
        .or(opts.inspect_brk.as_deref().map(|a| (a, true)))
    {
        let debuggee = agnijs_js_vm::make_debuggee(host.clone());
        let addr = inspect_addr(addr);
        let engine = inspect::Inspected::start(debuggee, &addr, title, wait).map_err(|e| {
            eprintln!("agnijs: cannot listen on {addr}: {e}");
            Failure(exit::USAGE)
        })?;
        Context::from_engine(Box::new(engine))
    } else {
        Context::with_host(host.clone())
    };
    let inspecting = opts.inspect.is_some() || opts.inspect_brk.is_some();
    let limits = Limits {
        fuel: opts.fuel,
        max_memory: opts.max_memory,
    };
    // Under a debugger, console calls keep their raw values for DevTools.
    Runtime::new(ctx, host, limits, !inspecting)
}

fn set_script_args(rt: &mut Runtime, args: Vec<String>) -> Result<(), Failure> {
    rt.ctx.set_global("scriptArgs", &args).map_err(|e| {
        eprintln!("agnijs: {e}");
        Failure(exit::USAGE)
    })
}

fn code_of(r: Result<Shown, Failure>) -> i32 {
    match r {
        Ok(_) => 0,
        Err(Failure(c)) => c,
    }
}

// ---- deciding script or module ------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Script,
    Module,
}

/// Flags first, then the extension (`.mjs`, `.cjs`), then the nearest
/// `package.json` `type`, then the syntax: code that only parses as a
/// module (it has `import` or `export`) is one.
fn decide_mode(rt: &mut Runtime, flags: &ModeOpts, path: Option<&str>, source: &str) -> Mode {
    if flags.module {
        return Mode::Module;
    }
    if flags.script {
        return Mode::Script;
    }
    if let Some(p) = path {
        if p.ends_with(".mjs") {
            return Mode::Module;
        }
        if p.ends_with(".cjs") {
            return Mode::Script;
        }
        if let Some(t) = package_type(p) {
            return t;
        }
    }
    let realm = rt.realm();
    let engine = rt.ctx.engine();
    if engine.compile_script(realm, "detect", source).is_err()
        && engine.compile_module(realm, "detect", source).is_ok()
    {
        return Mode::Module;
    }
    Mode::Script
}

/// The `type` of the nearest `package.json` above `path`.
fn package_type(path: &str) -> Option<Mode> {
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        if let Ok(text) = std::fs::read_to_string(d.join("package.json")) {
            let json: serde_json::Value = serde_json::from_str(&text).ok()?;
            return match json.get("type").and_then(|t| t.as_str()) {
                Some("module") => Some(Mode::Module),
                Some("commonjs") => Some(Mode::Script),
                _ => None,
            };
        }
        if d.file_name().is_some_and(|n| n == "node_modules") {
            return None;
        }
        dir = d.parent();
    }
    None
}

// ---- commands ---------------------------------------------------------

fn cmd_run(a: RunArgs) -> i32 {
    let file = a.file.clone().unwrap_or_else(|| "-".into());
    let (name, bytes) = if file == "-" {
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("agnijs: cannot read standard input: {e}");
            return exit::USAGE;
        }
        ("[stdin]".to_string(), buf)
    } else {
        match std::fs::read(&file) {
            Ok(b) => (absolute(&file), b),
            Err(e) => {
                eprintln!("agnijs: cannot read {file}: {e}");
                return exit::USAGE;
            }
        }
    };
    let mut rt = match make_runtime(&a.engine, &name) {
        Ok(rt) => rt,
        Err(Failure(c)) => return c,
    };
    let mut args = vec![file.clone()];
    args.extend(a.args);
    if let Err(Failure(c)) = set_script_args(&mut rt, args) {
        return c;
    }
    if host::is_bytecode(&bytes) {
        return code_of(rt.run_bytecode(&name, &bytes));
    }
    let Ok(source) = String::from_utf8(bytes) else {
        eprintln!("agnijs: {file} is not UTF-8 text");
        return exit::USAGE;
    };
    let path = (file != "-").then_some(name.as_str());
    match decide_mode(&mut rt, &a.mode, path, &source) {
        Mode::Script => code_of(rt.run_script(&name, &source)),
        Mode::Module => {
            if path.is_none() {
                rt.host
                    .add_virtual_module(&name, ModuleSource::Source(source.clone()));
            }
            code_of(rt.run_module(&name, &source))
        }
    }
}

fn cmd_eval(a: EvalArgs) -> i32 {
    let mut rt = match make_runtime(&a.engine, "[eval]") {
        Ok(rt) => rt,
        Err(Failure(c)) => return c,
    };
    if let Err(Failure(c)) = set_script_args(&mut rt, a.args) {
        return c;
    }
    let name = "[eval]";
    match decide_mode(&mut rt, &a.mode, None, &a.code) {
        Mode::Script => match rt.run_script(name, &a.code) {
            Ok(Shown::Value(v)) => {
                if a.print {
                    let text = rt.inspect(&v);
                    println!("{text}");
                }
                0
            }
            Ok(Shown::Nothing) => 0,
            Err(Failure(c)) => c,
        },
        Mode::Module => {
            rt.host
                .add_virtual_module(name, ModuleSource::Source(a.code.clone()));
            code_of(rt.run_module(name, &a.code))
        }
    }
}

fn cmd_repl(opts: EngineOpts) -> i32 {
    let mut rt = match make_runtime(&opts, "REPL") {
        Ok(rt) => rt,
        Err(Failure(c)) => return c,
    };
    if let Err(Failure(c)) = set_script_args(&mut rt, Vec::new()) {
        return c;
    }
    repl::run(&mut rt)
}

fn cmd_compile(a: CompileArgs) -> i32 {
    let source = match std::fs::read_to_string(&a.file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agnijs: cannot read {}: {e}", a.file);
            return exit::USAGE;
        }
    };
    let mut rt = match make_runtime(&EngineOpts::default(), "compile") {
        Ok(rt) => rt,
        Err(Failure(c)) => return c,
    };
    let name = absolute(&a.file);
    let mode = decide_mode(&mut rt, &a.mode, Some(&name), &source);
    let realm = rt.realm();
    let engine = rt.ctx.engine();
    // Compiled under the source's own path, so stack traces and relative
    // imports keep pointing at it.
    let r = match mode {
        Mode::Script => engine.compile_script(realm, &name, &source),
        Mode::Module => engine.compile_module(realm, &name, &source),
    };
    let bytes = match rt.check(r) {
        Ok(b) => b,
        Err(Failure(c)) => return c,
    };
    let out = a.output.unwrap_or_else(|| {
        Path::new(&a.file)
            .with_extension("jsc")
            .to_string_lossy()
            .into_owned()
    });
    if let Err(e) = std::fs::write(&out, &bytes) {
        eprintln!("agnijs: cannot write {out}: {e}");
        return exit::USAGE;
    }
    let kind = if mode == Mode::Module {
        "module"
    } else {
        "script"
    };
    eprintln!("agnijs: wrote {out} ({kind}, {} bytes)", bytes.len());
    0
}

fn cmd_snapshot(a: SnapshotArgs) -> i32 {
    // A plain engine: a snapshot cannot carry the prelude's host functions,
    // which `--snapshot` installs again after the restore.
    let host = make_host(&EngineOpts {
        regex_matcher: a.regex_matcher,
        ..EngineOpts::default()
    });
    let mut ctx = Context::with_host(host);
    let realm = ctx.realm();
    for file in &a.scripts {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("agnijs: cannot read {file}: {e}");
                return exit::USAGE;
            }
        };
        let name = absolute(file);
        let engine: &mut dyn Engine = ctx.engine();
        let r = engine
            .eval_script(realm, &name, &source)
            .and_then(|_| engine.run_jobs(realm));
        if let Err(e) = r {
            eprintln!("agnijs: {file}: {e}");
            return exit::UNCAUGHT;
        }
    }
    let bytes = match ctx.engine().snapshot(realm) {
        Ok(b) => b,
        Err(e @ api::Error::SnapshotUnsupported(_)) => {
            eprintln!("agnijs: {e}");
            return exit::UNCAUGHT;
        }
        Err(e) => {
            eprintln!("agnijs: cannot snapshot: {e}");
            return exit::UNCAUGHT;
        }
    };
    if let Err(e) = std::fs::write(&a.output, &bytes) {
        eprintln!("agnijs: cannot write {}: {e}", a.output);
        return exit::USAGE;
    }
    eprintln!("agnijs: wrote {} ({} bytes)", a.output, bytes.len());
    0
}
