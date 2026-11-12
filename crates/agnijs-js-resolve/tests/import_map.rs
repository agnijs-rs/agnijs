//! Import maps (ADR 0025): parsing and resolution, and a host whose
//! `resolve_module` goes through an import map, driven by a real engine.

use agnijs_engine_api::{Host, ImportAttribute, ModuleSource, PromiseState, PropertyKey, Value};
use agnijs_js_resolve::{ImportMap, ImportMapError};
use std::collections::HashMap;
use std::rc::Rc;

const BASE: &str = "https://app.example/index.html";

fn map(text: &str) -> ImportMap {
    ImportMap::parse(text, BASE).unwrap()
}

#[test]
fn bare_specifiers_map_to_urls_and_unmapped_ones_fail() {
    let m = map(
        r#"{"imports": {"moment": "/node_modules/moment/src/moment.js", "lodash": "https://cdn.example/lodash.js"}}"#,
    );
    assert_eq!(
        m.resolve("moment", BASE).unwrap(),
        "https://app.example/node_modules/moment/src/moment.js"
    );
    assert_eq!(
        m.resolve("lodash", BASE).unwrap(),
        "https://cdn.example/lodash.js"
    );
    assert_eq!(
        m.resolve("react", BASE),
        Err(ImportMapError::Unmapped {
            specifier: "react".into(),
            base_url: BASE.into()
        })
    );
    // URL-like specifiers no entry remaps resolve as URLs.
    assert_eq!(
        m.resolve("./a.js", "https://app.example/lib/b.js").unwrap(),
        "https://app.example/lib/a.js"
    );
    assert_eq!(
        m.resolve("https://x.example/y.js", BASE).unwrap(),
        "https://x.example/y.js"
    );
}

#[test]
fn trailing_slash_keys_map_packages_and_cannot_be_escaped() {
    let m = map(r#"{"imports": {"lib/": "/vendor/lib/", "lib": "/vendor/lib/index.js"}}"#);
    assert_eq!(
        m.resolve("lib", BASE).unwrap(),
        "https://app.example/vendor/lib/index.js"
    );
    assert_eq!(
        m.resolve("lib/a/b.js", BASE).unwrap(),
        "https://app.example/vendor/lib/a/b.js"
    );
    assert!(matches!(
        m.resolve("lib/../../secret.js", BASE),
        Err(ImportMapError::EscapesPackage { .. })
    ));
}

#[test]
fn url_keys_remap_urls_and_the_longest_prefix_wins() {
    let m = map(
        r#"{"imports": {"/app/": "/v2/app/", "/app/helpers/": "/v3/helpers/", "https://cdn.example/x.js": "/local/x.js"}}"#,
    );
    assert_eq!(
        m.resolve("/app/main.js", BASE).unwrap(),
        "https://app.example/v2/app/main.js"
    );
    assert_eq!(
        m.resolve("/app/helpers/h.js", BASE).unwrap(),
        "https://app.example/v3/helpers/h.js"
    );
    assert_eq!(
        m.resolve("https://cdn.example/x.js", BASE).unwrap(),
        "https://app.example/local/x.js"
    );
}

#[test]
fn scopes_apply_to_modules_under_their_prefix_before_imports() {
    let m = map(r#"{"imports": {"dep": "/dep-v1.js"},
            "scopes": {"/legacy/": {"dep": "/dep-v0.js"}, "/legacy/new/": {"dep": "/dep-v2.js"}, "/exact.js": {"dep": "/dep-exact.js"}}}"#);
    assert_eq!(
        m.resolve("dep", "https://app.example/main.js").unwrap(),
        "https://app.example/dep-v1.js"
    );
    assert_eq!(
        m.resolve("dep", "https://app.example/legacy/a.js").unwrap(),
        "https://app.example/dep-v0.js"
    );
    assert_eq!(
        m.resolve("dep", "https://app.example/legacy/new/a.js")
            .unwrap(),
        "https://app.example/dep-v2.js"
    );
    assert_eq!(
        m.resolve("dep", "https://app.example/exact.js").unwrap(),
        "https://app.example/dep-exact.js"
    );
    // A scope without a trailing slash matches only that exact URL.
    assert_eq!(
        m.resolve("dep", "https://app.example/exact.js/x").unwrap(),
        "https://app.example/dep-v1.js"
    );
}

#[test]
fn invalid_addresses_warn_and_block_their_specifier() {
    let m = map(
        r#"{"imports": {"a": 1, "b": "not a url", "c/": "/no-slash", "": "/empty.js", "ok": "/ok.js"}, "extra": true}"#,
    );
    assert_eq!(m.warnings().len(), 5, "{:?}", m.warnings());
    for s in ["a", "b", "c/x"] {
        assert!(
            matches!(m.resolve(s, BASE), Err(ImportMapError::Blocked { .. })),
            "{s}"
        );
    }
    assert_eq!(m.resolve("ok", BASE).unwrap(), "https://app.example/ok.js");
    assert_eq!(m.imports().count(), 4);
}

#[test]
fn malformed_maps_and_base_urls_are_errors() {
    assert!(matches!(
        ImportMap::parse("{imports: {}}", BASE),
        Err(ImportMapError::InvalidJson { .. })
    ));
    for text in [
        "[]",
        "null",
        r#"{"imports": []}"#,
        r#"{"scopes": {"/x/": 1}}"#,
        r#"{"integrity": "x"}"#,
    ] {
        assert!(
            matches!(
                ImportMap::parse(text, BASE),
                Err(ImportMapError::InvalidShape { .. })
            ),
            "{text}"
        );
    }
    assert!(matches!(
        ImportMap::parse("{}", "relative/base"),
        Err(ImportMapError::InvalidBaseUrl { .. })
    ));
    assert!(map("{}").is_empty());
    assert!(matches!(
        map("{}").resolve("x", "nope"),
        Err(ImportMapError::InvalidBaseUrl { .. })
    ));
}

#[test]
fn integrity_metadata_is_recorded_by_resolved_url() {
    let m =
        map(r#"{"imports": {"a": "/a.js"}, "integrity": {"/a.js": "sha384-abc", "./b.js": 7}}"#);
    let url = m.resolve("a", BASE).unwrap();
    assert_eq!(m.integrity(&url), Some("sha384-abc"));
    assert_eq!(m.integrity("https://app.example/b.js"), None);
    assert_eq!(m.warnings().len(), 1);
}

/// A host serving modules from memory by URL, with an import map in front.
struct MapHost {
    import_map: ImportMap,
    entry_base: String,
    files: HashMap<String, String>,
}

impl Host for MapHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        Ok(self
            .import_map
            .resolve(specifier, referrer.unwrap_or(&self.entry_base))?)
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        let text = self
            .files
            .get(resolved)
            .ok_or_else(|| format!("module not found: {resolved}"))?
            .clone();
        Ok(if resolved.ends_with(".json") {
            ModuleSource::Json(text)
        } else {
            ModuleSource::Source(text)
        })
    }
}

fn map_host() -> Rc<MapHost> {
    let import_map = ImportMap::parse(
        r#"{"imports": {"config": "/conf/app.json", "util/": "/lib/util/"},
            "scopes": {"/lib/": {"config": "/conf/lib.json"}}}"#,
        "https://app.example/",
    )
    .unwrap();
    let files = [
        (
            "https://app.example/main.js",
            "import config from 'config' with { type: 'json' };\n\
             import { tag } from 'util/tag.js';\n\
             export default config.name + ',' + tag;",
        ),
        (
            "https://app.example/lib/util/tag.js",
            "import c from 'config' with { type: 'json' }; export const tag = 'tag:' + c.name;",
        ),
        ("https://app.example/conf/app.json", r#"{"name": "app"}"#),
        ("https://app.example/conf/lib.json", r#"{"name": "lib"}"#),
        ("https://app.example/bare.js", "import 'not-mapped';"),
    ];
    Rc::new(MapHost {
        import_map,
        entry_base: "https://app.example/".into(),
        files: files
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    })
}

#[test]
fn an_engine_loads_a_graph_through_the_import_map_with_json_modules() {
    let h = map_host();
    let mut engine = agnijs_js_vm::make_engine(h.clone());
    let realm = engine.main_realm();
    let p = engine.load_module(realm, "/main.js").unwrap();
    engine.run_jobs(realm).unwrap();
    let result = match engine.promise_state(realm, &p).unwrap() {
        PromiseState::Fulfilled(ns) => engine
            .get(realm, &ns, &PropertyKey::from("default"))
            .unwrap(),
        other => panic!("{other:?}"),
    };
    // main.js sees the top-level "config"; lib/util/tag.js the /lib/ scope's.
    assert_eq!(result, Value::string("app,tag:lib"));
}

#[test]
fn an_unmapped_bare_specifier_rejects_the_load_with_the_import_map_message() {
    let h = map_host();
    let mut engine = agnijs_js_vm::make_engine(h.clone());
    let realm = engine.main_realm();
    let p = engine.load_module(realm, "/bare.js").unwrap();
    engine.run_jobs(realm).unwrap();
    let PromiseState::Rejected(reason) = engine.promise_state(realm, &p).unwrap() else {
        panic!("not rejected")
    };
    let message = engine
        .get(realm, &reason, &PropertyKey::from("message"))
        .unwrap();
    assert_eq!(
        message,
        Value::string("bare specifier 'not-mapped' imported from https://app.example/bare.js is not mapped by the import map")
    );
}
