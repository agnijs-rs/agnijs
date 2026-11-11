//! The resolver against the fixture tree in `tests/fixtures/project`.
//!
//! Layout (see also the module docs of the crate):
//!
//! ```text
//! package.json                 name "app"; imports #util, #internal/*, #dep, #bad; exports . and ./feature
//! src/main.js util.js feature.js data.json
//! src/internal/a.js            target of #internal/*
//! src/lib/index.js             directory with an index
//! src/pkgdir/                  package.json "main": "./entry" + entry.js
//! src/deep/file.js             importer; src/deep/node_modules/plain shadows node_modules/plain
//! node_modules/plain           "main": "lib/main.js", plus other.js (no exports: deep paths allowed)
//! node_modules/plain-index     index.js only
//! node_modules/dep-main        "main": "main.js"
//! node_modules/modern          exports: conditions, ./sub/* pattern, a null-blocked subtree, array, escape
//! node_modules/condorder       exports conditions with "default" before "import"
//! node_modules/@scope/pkg      exports as a bare string
//! ```

use agnijs_js_resolve::{Options, ResolveError, Resolver, StdFs};

fn root() -> String {
    format!("{}/tests/fixtures/project", env!("CARGO_MANIFEST_DIR")).replace('\\', "/")
}

fn resolver() -> Resolver<StdFs> {
    Resolver::new(StdFs, Options::default())
}

/// Resolve from the fixture file `from` (relative to the project root) and
/// return the result relative to the root.
fn ok(spec: &str, from: &str) -> String {
    ok_with(&resolver(), spec, from)
}

fn ok_with(r: &Resolver<StdFs>, spec: &str, from: &str) -> String {
    let root = root();
    match r.resolve(spec, &format!("{root}/{from}")) {
        Ok(p) => p.strip_prefix(&format!("{root}/")).unwrap_or(&p).to_string(),
        Err(e) => panic!("{spec} from {from}: {e}"),
    }
}

fn err(spec: &str, from: &str) -> ResolveError {
    resolver()
        .resolve(spec, &format!("{}/{from}", root()))
        .expect_err(spec)
}

#[test]
fn relative_specifiers_find_files_with_and_without_extensions() {
    assert_eq!(ok("./util.js", "src/main.js"), "src/util.js");
    assert_eq!(ok("./util", "src/main.js"), "src/util.js");
    assert_eq!(ok("./data", "src/main.js"), "src/data.json");
    assert_eq!(ok("../util", "src/lib/index.js"), "src/util.js");
    assert_eq!(ok("./internal/a", "src/main.js"), "src/internal/a.js");
}

#[test]
fn directories_resolve_through_main_then_index() {
    assert_eq!(ok("./lib", "src/main.js"), "src/lib/index.js");
    assert_eq!(ok("./lib/", "src/main.js"), "src/lib/index.js");
    assert_eq!(ok("./pkgdir", "src/main.js"), "src/pkgdir/entry.js");
    assert_eq!(ok(".", "src/lib/index.js"), "src/lib/index.js");
}

#[test]
fn absolute_specifiers_are_taken_as_they_are() {
    let abs = format!("{}/src/util", root());
    assert_eq!(ok(&abs, "src/main.js"), "src/util.js");
}

#[test]
fn missing_files_are_not_found_errors_naming_the_specifier() {
    let e = err("./nope", "src/main.js");
    assert!(matches!(e, ResolveError::NotFound { .. }), "{e:?}");
    assert!(e.to_string().contains("./nope"), "{e}");
}

#[test]
fn with_no_extensions_configured_only_exact_files_resolve() {
    let r = Resolver::new(StdFs, Options { extensions: vec![], ..Options::default() });
    assert_eq!(ok_with(&r, "./util.js", "src/main.js"), "src/util.js");
    let from = format!("{}/src/main.js", root());
    assert!(r.resolve("./util", &from).is_err());
    assert!(r.resolve("./lib", &from).is_err());
}

#[test]
fn bare_specifiers_walk_up_node_modules_nearest_first() {
    assert_eq!(ok("plain", "src/main.js"), "node_modules/plain/lib/main.js");
    assert_eq!(ok("plain-index", "src/main.js"), "node_modules/plain-index/index.js");
    assert_eq!(ok("plain", "src/deep/file.js"), "src/deep/node_modules/plain/index.js");
    assert_eq!(ok("plain-index", "src/deep/file.js"), "node_modules/plain-index/index.js");
}

#[test]
fn packages_without_exports_allow_deep_paths_and_extension_lookup() {
    assert_eq!(ok("plain/other.js", "src/main.js"), "node_modules/plain/other.js");
    assert_eq!(ok("plain/other", "src/main.js"), "node_modules/plain/other.js");
    // `lib` is a directory with no package.json and no index.
    assert!(matches!(err("plain/lib", "src/main.js"), ResolveError::NotFound { .. }));
}

#[test]
fn a_missing_package_is_not_found() {
    assert!(matches!(err("ghost", "src/main.js"), ResolveError::NotFound { .. }));
}

#[test]
fn scoped_packages_resolve_with_a_string_exports() {
    assert_eq!(ok("@scope/pkg", "src/main.js"), "node_modules/@scope/pkg/main.js");
}

#[test]
fn exports_pick_by_condition_and_the_default_set_is_import_and_default() {
    assert_eq!(ok("modern", "src/main.js"), "node_modules/modern/esm/index.js");
    let r = Resolver::new(
        StdFs,
        Options { conditions: vec!["require".into(), "default".into()], ..Options::default() },
    );
    assert_eq!(ok_with(&r, "modern", "src/main.js"), "node_modules/modern/cjs/index.js");
}

#[test]
fn condition_order_is_the_package_authors_key_order() {
    assert_eq!(ok("condorder", "src/main.js"), "node_modules/condorder/d.js");
}

#[test]
fn exports_subpaths_and_star_patterns() {
    assert_eq!(ok("modern/sub/a", "src/main.js"), "node_modules/modern/esm/sub/a.js");
    assert_eq!(ok("modern/package.json", "src/main.js"), "node_modules/modern/package.json");
    assert_eq!(ok("modern/list", "src/main.js"), "node_modules/modern/esm/list.js");
}

#[test]
fn exports_block_unlisted_and_null_targets() {
    for spec in ["modern/hidden.js", "modern/sub/private/x", "modern/nothing"] {
        let e = err(spec, "src/main.js");
        assert!(matches!(e, ResolveError::PackagePathNotExported { .. }), "{spec}: {e:?}");
    }
}

#[test]
fn exports_targets_may_not_climb_out_of_the_package() {
    let e = err("modern/escape", "src/main.js");
    assert!(matches!(e, ResolveError::InvalidPackageTarget { .. }), "{e:?}");
}

#[test]
fn a_package_can_import_itself_by_name_through_its_exports() {
    assert_eq!(ok("app", "src/main.js"), "src/main.js");
    assert_eq!(ok("app/feature", "src/lib/index.js"), "src/feature.js");
    assert!(matches!(err("app/util", "src/main.js"), ResolveError::PackagePathNotExported { .. }));
}

#[test]
fn imports_map_hash_specifiers() {
    assert_eq!(ok("#util", "src/lib/index.js"), "src/util.js");
    assert_eq!(ok("#internal/a", "src/lib/index.js"), "src/internal/a.js");
    assert_eq!(ok("#dep", "src/main.js"), "node_modules/dep-main/main.js");
    assert!(matches!(err("#nothing", "src/main.js"), ResolveError::PackageImportNotDefined { .. }));
    assert!(matches!(err("#bad", "src/main.js"), ResolveError::InvalidPackageTarget { .. }));
}

#[test]
fn malformed_specifiers_are_invalid_not_panics() {
    for spec in ["", "#", "@scope", "@/x", "plain/../x"] {
        let e = resolver().resolve(spec, &format!("{}/src/main.js", root()));
        assert!(e.is_err(), "{spec:?} resolved to {e:?}");
    }
}
