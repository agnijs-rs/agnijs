//! The resolver driven from a real `Host`: an embedder's `resolve_module`
//! calls it and `load_module` reads the resolved file.

use agnijs_engine_api::{Host, ImportAttribute, ModuleSource, PromiseState, PropertyKey, Value};
use agnijs_js_resolve::{Options, Resolver, StdFs};
use std::rc::Rc;

struct NodeHost {
    resolver: Resolver<StdFs>,
    root: String,
}

impl Host for NodeHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        let r = match referrer {
            Some(from) => self.resolver.resolve(specifier, from),
            None => self.resolver.resolve_from_dir(specifier, &self.root),
        };
        r.map_err(String::from)
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        let text = std::fs::read_to_string(resolved).map_err(|e| e.to_string())?;
        Ok(if resolved.ends_with(".json") { ModuleSource::Json(text) } else { ModuleSource::Source(text) })
    }
}

fn host() -> Rc<NodeHost> {
    let root = format!("{}/tests/fixtures/project", env!("CARGO_MANIFEST_DIR")).replace('\\', "/");
    Rc::new(NodeHost { resolver: Resolver::new(StdFs, Options::default()), root })
}

#[test]
fn an_entry_module_imports_packages_files_and_hash_specifiers() {
    let h = host();
    let entry = format!("{}/src/entry.mjs", h.root);
    let result = {
        let mut engine = agnijs_js_vm::make_engine(h.clone());
        let realm = engine.main_realm();
        // modern/esm files are ES modules; plain's main too.
        let p = engine.load_module(realm, &entry).unwrap();
        engine.run_jobs(realm).unwrap();
        match engine.promise_state(realm, &p).unwrap() {
            PromiseState::Fulfilled(ns) => engine.get(realm, &ns, &PropertyKey::from("default")).unwrap(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(result, Value::string("modern-esm,modern-sub-a,util,1,plain"));
}

#[test]
fn an_unresolvable_import_rejects_the_load_with_the_resolver_message() {
    let h = host();
    let mut engine = agnijs_js_vm::make_engine(h.clone());
    let realm = engine.main_realm();
    let p = engine.load_module(realm, "ghost-package").unwrap();
    engine.run_jobs(realm).unwrap();
    assert!(matches!(engine.promise_state(realm, &p).unwrap(), PromiseState::Rejected(_)));
}
