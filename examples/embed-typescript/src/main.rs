//! An embedder whose modules are TypeScript. The host's `load_module`
//! transpiles each `.ts` file with agnijs-js-typescript and hands the engine
//! the JavaScript with its source map, so stack traces point into the
//! TypeScript. A file the transpiler refuses fails its import with the
//! diagnostics. docs/embedding-guide.md walks through it.
//!
//! Run it with `cargo run -p agnijs-example-typescript`; `tests/output.rs`
//! checks what it prints.

use agnijs::api::{
    ConsoleLevel, Error, Host, ImportAttribute, ModuleSource, PromiseState, RealmId,
};
use agnijs_js_typescript::{transpile, Options};
use std::rc::Rc;

/// The files this program serves, by canonical name.
const FILES: &[(&str, &str)] = &[
    (
        "app/main.ts",
        "import { Account, Currency } from './account.ts';\n\
         const a: Account = new Account('ada', Currency.Eur);\n\
         a.deposit(30);\n\
         console.log(a.describe());\n\
         export function overdraw(): void {\n\
         \x20 a.withdraw(100);\n\
         }\n",
    ),
    (
        "app/account.ts",
        "export enum Currency { Eur = 'EUR', Usd = 'USD' }\n\
         export class Account {\n\
         \x20 private balance: number = 0;\n\
         \x20 constructor(readonly owner: string, readonly currency: Currency) {}\n\
         \x20 deposit(amount: number): void { this.balance += amount; }\n\
         \x20 withdraw(amount: number): void {\n\
         \x20   if (amount > this.balance) {\n\
         \x20     throw new RangeError(`${this.owner} has ${this.balance} ${this.currency}`);\n\
         \x20   }\n\
         \x20   this.balance -= amount;\n\
         \x20 }\n\
         \x20 describe(): string { return `${this.owner}: ${this.balance} ${this.currency}`; }\n\
         }\n",
    ),
    // `accessor` fields are refused (ADR 0029): the import fails with the
    // diagnostic, positions in this file.
    (
        "app/broken.ts",
        "export class Box {\n  accessor value = 1;\n}\n",
    ),
];

struct App;

impl Host for App {
    fn console(&self, _realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        let level = format!("{level:?}").to_lowercase();
        println!("console.{level}: {}", args.join(" "));
    }

    fn resolve_module(
        &self,
        _referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(format!("app/{}", specifier.trim_start_matches("./")))
    }

    /// TypeScript in, JavaScript and its map out. `Options::for_path`
    /// picks the settings from the extension (and is `None` for a `.d.ts`,
    /// which holds no code).
    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        let (_, source) = FILES
            .iter()
            .find(|(name, _)| *name == resolved)
            .ok_or_else(|| format!("module not found: {resolved}"))?;
        let options = Options::for_path(resolved)
            .ok_or_else(|| format!("{resolved} is a declaration file"))?;
        // The error's text is the diagnostics, one per line with its
        // position; the engine rejects the import with it.
        let out = transpile(resolved, source, &options).map_err(|e| e.to_string())?;
        Ok(ModuleSource::SourceWithMap {
            source: out.code,
            source_map: out.source_map,
        })
    }
}

fn main() -> Result<(), Error> {
    let mut engine = agnijs::new_engine(Rc::new(App));
    let realm = engine.main_realm();
    engine.set_memory_limit(Some(64 << 20));

    let main = engine.load_module(realm, "main.ts")?;
    engine.run_jobs(realm)?;
    let PromiseState::Fulfilled(namespace) = engine.promise_state(realm, &main)? else {
        panic!("main.ts did not load");
    };

    // An exception thrown in TypeScript reports TypeScript positions.
    let overdraw = engine.get(realm, &namespace, &"overdraw".into())?;
    match engine.call(realm, &overdraw, &agnijs::api::Value::Undefined, &[]) {
        Err(Error::Exception(e)) => {
            println!("exception: {}: {}", e.name, e.message);
            for f in e.stack.iter().take(2) {
                println!("  at {}:{}:{}", f.script, f.line, f.column);
            }
        }
        other => println!("expected an exception, got {other:?}"),
    }

    // A file the transpiler refuses: the import rejects with the diagnostic.
    let broken = engine.load_module(realm, "broken.ts")?;
    engine.run_jobs(realm)?;
    if let PromiseState::Rejected(reason) = engine.promise_state(realm, &broken)? {
        println!("import failed: {}", engine.to_string(realm, &reason)?);
    }
    Ok(())
}
