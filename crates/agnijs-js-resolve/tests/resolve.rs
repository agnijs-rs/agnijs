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

use agnijs_js_resolve::{FileSystem, Options, ResolveError, Resolver, StdFs};

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
    // The path join treats `\` as a separator, so the segment checks do too,
    // on every platform; encoded dots and separators and Windows' trailing
    // dots and spaces are refused the same way (TASK-103.38).
    for spec in [
        r"modern/sub/..\..\..\..\src/util",
        r"modern/sub/..\..\x",
        r"modern/sub/a\..\..\..\..\src/util",
        "modern/sub/%2e%2e/%2e%2e/%2e%2e/%2e%2e/src/util",
        "modern/sub/%2E./%2E./%2E./%2E./src/util",
        "modern/sub/..%5c..%5c..%5c..%5csrc/util",
        "modern/sub/..%2f..%2f..%2f..%2fsrc/util",
        "modern/sub/.. /.. /.. /.. /src/util",
        "modern/sub/..../x",
        "modern/sub/NODE_MODULES./plain/other",
        r"#internal/..\..\src/util",
        r"#internal/..\..\..\x",
        "#internal/%2e%2e/%2e%2e/src/util",
        r"app/feature\..\x",
    ] {
        let e = err(spec, "src/main.js");
        assert!(
            matches!(
                e,
                ResolveError::InvalidPackageTarget { .. }
                    | ResolveError::InvalidSpecifier { .. }
                    | ResolveError::PackagePathNotExported { .. }
            ),
            "{spec}: {e:?}"
        );
    }
}

/// A file system where every path that is not a `package.json` is a file:
/// whatever path the resolver builds, it gets back. Only the containment
/// checks stand between a specifier and a file outside the package.
struct EverythingIsAFile;

impl FileSystem for EverythingIsAFile {
    fn is_file(&self, path: &str) -> bool {
        if path.ends_with("/package.json") {
            StdFs.is_file(path)
        } else {
            !path.ends_with('/')
        }
    }
    fn is_dir(&self, path: &str) -> bool {
        StdFs.is_dir(path)
    }
    fn read_to_string(&self, path: &str) -> Option<String> {
        StdFs.read_to_string(path)
    }
}

/// A segment that a file system could take as `.` or `..` (or as
/// `node_modules` when `node_modules` is set):
/// after percent-decoding and dropping the trailing dots and spaces Windows
/// ignores.
fn sneaky_segment(seg: &str, node_modules: bool) -> bool {
    let lower = seg
        .to_ascii_lowercase()
        .replace("%2e", ".")
        .replace("%20", " ");
    let t = lower.trim_end_matches(['.', ' ']);
    t.is_empty() || (node_modules && t == "node_modules")
}

#[test]
fn fuzzed_path_forms_never_leave_the_package() {
    let r = Resolver::new(EverythingIsAFile, Options::default());
    let root = root();
    let from = format!("{root}/src/main.js");
    // (specifier prefix, the directory every result must stay inside, whether
    // it goes through exports or imports: a legacy package may hold its own
    // node_modules)
    let packages = [
        ("modern/sub/", format!("{root}/node_modules/modern/"), true),
        ("plain/", format!("{root}/node_modules/plain/"), false),
        (
            "@scope/pkg/",
            format!("{root}/node_modules/@scope/pkg/"),
            true,
        ),
        ("#internal/", format!("{root}/src/internal/"), true),
        ("app/", format!("{root}/"), true),
    ];
    let pieces = [
        "..",
        ".",
        "...",
        ".. ",
        ". ",
        "%2e%2e",
        "%2E.",
        ".%2e",
        "%2e",
        "node_modules",
        "NODE_MODULES.",
        "node_modules ",
        "C:",
        "C:..",
        "c:\\",
        "\\\\srv\\share",
        "\\\\?\\C:",
        "//",
        "%2f",
        "%5c",
        "%5C..",
        "x",
        "src",
        "util",
        "a",
        "..:$DATA",
        "",
        " ",
        "\u{0}",
        "\u{e9}",
    ];
    let seps = ["/", "\\", "\\/", "/\\"];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let mut resolved = 0;
    for _ in 0..30_000 {
        let (prefix, inside, strict) = &packages[next(packages.len())];
        let mut spec = prefix.to_string();
        for i in 0..1 + next(5) {
            if i > 0 {
                spec.push_str(seps[next(seps.len())]);
            }
            spec.push_str(pieces[next(pieces.len())]);
        }
        let result = std::panic::catch_unwind(|| r.resolve(&spec, &from));
        let Ok(result) = result else {
            panic!("{spec:?} panicked")
        };
        let Ok(path) = result else { continue };
        resolved += 1;
        // The directory itself (`plain///` is the package root) is inside.
        let rest = if path == inside.trim_end_matches('/') {
            Some("")
        } else {
            path.strip_prefix(inside.as_str())
        };
        assert!(rest.is_some(), "{spec:?} resolved outside {inside}: {path}");
        let rest = rest.unwrap();
        assert!(
            rest.is_empty() || !rest.split(['/', '\\']).any(|s| sneaky_segment(s, *strict)),
            "{spec:?} resolved to {path}"
        );
        assert!(!rest.contains(':'), "{spec:?} resolved to {path}");
    }
    assert!(resolved > 1_000, "only {resolved} specifiers resolved");
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
    for spec in [
        "",
        "#",
        "@scope",
        "@/x",
        "plain/../x",
        r"plain/..\..\src/util",
        r"plain/a\..\..\..\src/util",
        r"plain\..\..\src/util",
        "plain/%2e%2e/%2e%2e/src/util",
        "plain/..%2f..%2fsrc/util",
        "plain/.. /.. /src/util",
        "plain/.../x",
        "@scope/../plain/other",
        r"@scope/..\plain/other",
        "@../plain/other",
        r"#internal/..\..\src/util",
    ] {
        let e = resolver().resolve(spec, &format!("{}/src/main.js", root()));
        assert!(e.is_err(), "{spec:?} resolved to {e:?}");
    }
}
