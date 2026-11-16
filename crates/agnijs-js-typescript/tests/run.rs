//! The output runs on the engine and means what the TypeScript meant:
//! modules through a `Host::load_module` that transpiles, scripts through
//! `eval_script`.

use agnijs_engine_api::{Host, ImportAttribute, ModuleSource, PromiseState, PropertyKey, Value};
use agnijs_js_typescript::{transpile, Options, SourceKind};
use std::collections::HashMap;
use std::rc::Rc;

/// A host whose modules are TypeScript, transpiled on load.
struct TsHost {
    files: HashMap<&'static str, &'static str>,
    options: Options,
}

impl Host for TsHost {
    fn resolve_module(
        &self,
        _: Option<&str>,
        specifier: &str,
        _: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(specifier.trim_start_matches("./").to_string())
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        let ts = self
            .files
            .get(resolved)
            .ok_or_else(|| format!("no module {resolved}"))?;
        let out = transpile(resolved, ts, &self.options).map_err(|e| e.to_string())?;
        Ok(ModuleSource::SourceWithMap {
            source: out.code,
            source_map: out.source_map,
        })
    }
}

/// Loads `main.ts` and returns its `default` export.
fn run_module(files: &[(&'static str, &'static str)], options: Options) -> Value {
    let host = Rc::new(TsHost {
        files: files.iter().copied().collect(),
        options,
    });
    let mut engine = agnijs_js_vm::make_engine(host);
    let realm = engine.main_realm();
    let p = engine.load_module(realm, "main.ts").unwrap();
    engine.run_jobs(realm).unwrap();
    match engine.promise_state(realm, &p).unwrap() {
        PromiseState::Fulfilled(ns) => engine
            .get(realm, &ns, &PropertyKey::from("default"))
            .unwrap(),
        PromiseState::Rejected(e) => {
            let message = engine
                .get(realm, &e, &PropertyKey::from("message"))
                .unwrap();
            panic!("main.ts rejected: {message:?}")
        }
        other => panic!("{other:?}"),
    }
}

fn module(source: &'static str) -> Value {
    run_module(&[("main.ts", source)], Options::default())
}

#[test]
fn enums_have_values_and_reverse_mappings() {
    let v = module(
        "enum D { Up = 1, Down, Left = Up << 4 }\n\
         const enum C { A = 2 }\n\
         enum S { Yes = 'yes' }\n\
         export default JSON.stringify([D.Down, D[2], D.Left, C.A, S.Yes, S['yes']]);",
    );
    assert_eq!(v, Value::string("[2,\"Down\",16,2,\"yes\",null]"));
}

#[test]
fn namespaces_merge_and_nest() {
    let v = module(
        "namespace G { export const a = 1; export namespace In.Deep { export const b = a + 1; } }\n\
         namespace G { export const c = G.In.Deep.b + 1; }\n\
         import b = G.In.Deep.b;\n\
         export default [G.a, b, G.c].join();",
    );
    assert_eq!(v, Value::string("1,2,3"));
}

#[test]
fn parameter_properties_are_assigned_before_the_body_and_after_super() {
    let v = module(
        "class Base { constructor(public name: string) {} }\n\
         class Child extends Base { tag = this.name + '!'; constructor(name: string, private age: number) { super(name); } get() { return this.tag + this.age; } }\n\
         export default new Child('x', 3).get();",
    );
    assert_eq!(v, Value::string("x!3"));
}

#[test]
fn types_and_type_only_imports_are_gone() {
    let v = run_module(
        &[
            ("main.ts", "import { type T, value } from './dep.ts';\nimport type { U } from './missing.ts';\nlet x: T & U = value as any;\nexport default x satisfies number;"),
            ("dep.ts", "export type T = number;\nexport const value: T = 7;"),
        ],
        Options::default(),
    );
    assert_eq!(v, Value::Number(7.0));
}

#[test]
fn legacy_decorators_run_with_the_helpers() {
    let mut options = Options::default();
    options.experimental_decorators = true;
    options.emit_decorator_metadata = true;
    let v = run_module(
        &[(
            "main.ts",
            "\"use strict\";\n\
             const log: string[] = [];\n\
             function cls<T extends Function>(t: T) { log.push('class ' + t.name); return t; }\n\
             function method(t: any, k: string, d: PropertyDescriptor) { log.push('method ' + k); const f = d.value; d.value = function (...a: any[]) { return f.apply(this, a) * 2; }; return d; }\n\
             function param(t: any, k: string, i: number) { log.push('param ' + k + ' ' + i); }\n\
             function prop(t: any, k: string) { log.push('prop ' + k); }\n\
             @cls class A { @prop field = 1; @method twice(@param x: number) { return x; } }\n\
             export default log.join(', ') + ' = ' + new A().twice(21);",
        )],
        options,
    );
    assert_eq!(
        v,
        Value::string("prop field, param twice 0, method twice, class A = 42")
    );
}

#[test]
fn classic_jsx_calls_the_factory() {
    let mut options = Options::for_path("main.tsx").unwrap();
    options.jsx_factory = "h".into();
    options.jsx_fragment_factory = "Frag".into();
    let v = run_module(
        &[(
            "main.ts",
            "const Frag = 'frag';\n\
             function h(tag: any, props: any, ...children: any[]) { return [tag, props && Object.keys(props).join('|'), ...children].join(':'); }\n\
             const name: string = 'n';\n\
             export default <div id='a' title={name}><>{name}</></div>;",
        )],
        options,
    );
    assert_eq!(v, Value::string("div:id|title:frag::n"));
}

#[test]
fn a_script_runs_as_a_script() {
    let mut options = Options::for_path("lib.cts").unwrap();
    assert_eq!(options.kind, SourceKind::Script);
    options.experimental_decorators = true;
    let out = transpile(
        "lib.cts",
        "#!/usr/bin/env agnijs\n\"use strict\";\nfunction d(t: any) { t.marked = true; }\n@d class K {}\nvar result: boolean = (K as any).marked && (function (this: unknown) { return this === undefined; })();\nresult;",
        &options,
    )
    .unwrap();
    let mut engine = agnijs_js_vm::make_engine(Rc::new(TsHost {
        files: HashMap::new(),
        options: Options::default(),
    }));
    let realm = engine.main_realm();
    // The helper line follows the prologue, so the script is still strict.
    assert_eq!(
        engine.eval_script(realm, "lib.cts", &out.code).unwrap(),
        Value::Bool(true)
    );
}
