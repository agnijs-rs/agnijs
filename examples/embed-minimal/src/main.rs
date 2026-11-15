//! The smallest program that embeds AgniJS through its full engine
//! interface: it runs a script, exposes a Rust function to JavaScript, loads
//! a module graph through its own host, drives the job loop and reads an
//! exception. docs/embedding-guide.md walks through it.
//!
//! Run it with `cargo run -p agnijs-example-embed`; `tests/output.rs` checks
//! what it prints.

use agnijs::api::{
    CallContext, ConsoleLevel, Engine, Error, Host, ImportAttribute, ModuleSource, PromiseState,
    RealmId, Value,
};
use std::rc::Rc;

/// The modules this program serves, by canonical name.
const MODULES: &[(&str, &str)] = &[
    (
        "app/main.js",
        "import { double } from './math.js';\n\
         console.log('double(21) is', double(21));\n\
         export const answer = double(21);\n",
    ),
    ("app/math.js", "export const double = (n) => add(n, n);\n"),
];

/// The host: where console output goes and where modules come from. Every
/// hook has a default, so a host implements only the ones it uses.
struct App;

impl Host for App {
    fn console(&self, _realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        let level = format!("{level:?}").to_lowercase();
        println!("console.{level}: {}", args.join(" "));
    }

    /// Specifiers name files under `app/`, relative or bare.
    fn resolve_module(
        &self,
        _referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(format!("app/{}", specifier.trim_start_matches("./")))
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        MODULES
            .iter()
            .find(|(name, _)| *name == resolved)
            .map(|(_, source)| ModuleSource::Source(source.to_string()))
            .ok_or_else(|| format!("module not found: {resolved}"))
    }
}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    // A host that runs code it does not trust sets limits first
    // (docs/security-model.md).
    engine.set_memory_limit(Some(64 << 20));

    // A script: its completion value comes back as a Value.
    let sum = engine.eval_script(realm, "sum.js", "1 + 2")?;
    println!("script: 1 + 2 = {}", engine.to_string(realm, &sum)?);

    // A host function, installed as a global.
    let add = engine.new_function(
        realm,
        "add",
        Rc::new(|engine: &mut dyn Engine, cx: CallContext| {
            let a = engine.to_number(cx.realm, &cx.arg(0))?;
            let b = engine.to_number(cx.realm, &cx.arg(1))?;
            Ok(Value::Number(a + b))
        }),
    )?;
    let global = engine.global_object(realm)?;
    engine.set(realm, &global, &"add".into(), &add)?;
    let sum = engine.eval_script(realm, "add.js", "add(2, 40)")?;
    println!("host function: add(2, 40) = {}", engine.to_string(realm, &sum)?);

    // A module graph: load_module returns a promise for the namespace,
    // which settles as the job loop runs.
    let promise = engine.load_module(realm, "main.js")?;
    run_job_loop(&mut *engine, realm)?;
    match engine.promise_state(realm, &promise)? {
        PromiseState::Fulfilled(namespace) => {
            let answer = engine.get(realm, &namespace, &"answer".into())?;
            println!("module: answer = {}", engine.to_string(realm, &answer)?);
        }
        PromiseState::Rejected(reason) => {
            println!("module failed: {}", engine.to_string(realm, &reason)?)
        }
        PromiseState::Pending => println!("module still loading"),
    }

    // Promise reactions run only when the host runs the job loop.
    engine.eval_script(
        realm,
        "jobs.js",
        "globalThis.order = [];\n\
         Promise.resolve().then(() => order.push('job'));\n\
         order.push('script');\n",
    )?;
    let ran = run_job_loop(&mut *engine, realm)?;
    let order = engine.eval_script(realm, "order.js", "order.join(', ')")?;
    println!("jobs: ran {ran}, order {}", engine.to_string(realm, &order)?);

    // A thrown exception comes back as Error::Exception, read out.
    let source = "function check(n) {\n  if (n < 0) throw new RangeError('negative');\n}\ncheck(-1);\n";
    match engine.eval_script(realm, "check.js", source) {
        Err(Error::Exception(e)) => {
            let top = &e.stack[0];
            println!(
                "exception: {}: {} at {} ({}:{})",
                e.name, e.message, top.function, top.script, top.line
            );
        }
        other => println!("expected an exception, got {other:?}"),
    }
    Ok(())
}

/// Run jobs until none are left. An event loop does this after every task
/// (a timer, an I/O completion) it runs.
fn run_job_loop(engine: &mut dyn Engine, realm: RealmId) -> Result<usize, Error> {
    let mut total = 0;
    loop {
        match engine.run_jobs(realm)? {
            0 => return Ok(total),
            n => total += n,
        }
    }
}
