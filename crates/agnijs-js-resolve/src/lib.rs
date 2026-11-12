//! A Node-style module resolver for embedders (ADR 0023).
//!
//! The engine asks its host to turn `(referrer, specifier)` into a canonical
//! specifier (`Host::resolve_module`). An embedder that runs code from npm
//! packages calls [`Resolver::resolve`] from there. This crate depends on no
//! engine crate, and no engine crate depends on it.
//!
//! What it implements, over a [`FileSystem`]:
//!
//! - relative (`./x`, `../x`) and absolute specifiers, with extension
//!   resolution and directory resolution (`package.json` `main`, then
//!   `index`), both controlled by [`Options::extensions`];
//! - bare specifiers found by walking up `node_modules`, scoped packages
//!   included;
//! - package `exports` (strings, arrays, condition objects in key order,
//!   subpaths, one-`*` patterns, `null` targets) and self-reference by the
//!   package's own name;
//! - package `imports` (`#specifier`).
//!
//! Paths are strings with `/` separators. Results are never canonicalised
//! through symlinks.
//!
//! The [`ImportMap`] type is a second, independent resolver: WHATWG import
//! maps over URLs (ADR 0025).

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;

mod import_map;
pub use import_map::{ImportMap, ImportMapError};

/// The file-system operations the resolver needs.
pub trait FileSystem {
    fn is_file(&self, path: &str) -> bool;
    fn is_dir(&self, path: &str) -> bool;
    fn read_to_string(&self, path: &str) -> Option<String>;
}

/// [`FileSystem`] over `std::fs`.
pub struct StdFs;

impl FileSystem for StdFs {
    fn is_file(&self, path: &str) -> bool {
        std::fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
    }
    fn is_dir(&self, path: &str) -> bool {
        std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
    }
    fn read_to_string(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Conditions matched against `exports` and `imports` condition objects
    /// (`default` always matches). Default: `import`.
    pub conditions: Vec<String>,
    /// Extensions tried, in order, on a path that is not a file, and the
    /// switch for directory resolution. Empty: only exact files resolve.
    pub extensions: Vec<String>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            conditions: vec!["import".into()],
            extensions: [".js", ".mjs", ".cjs", ".json"].map(String::from).to_vec(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// Nothing matched on disk.
    NotFound { specifier: String, from: String },
    InvalidSpecifier { specifier: String },
    /// The package's `exports` does not expose this subpath.
    PackagePathNotExported { package: String, subpath: String },
    /// No `imports` entry for this `#specifier`.
    PackageImportNotDefined { specifier: String, package_dir: String },
    /// A target that is not a `./` path inside the package.
    InvalidPackageTarget { package: String, target: String },
    /// A `package.json` that cannot be read or is malformed.
    InvalidPackageConfig { path: String, message: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound { specifier, from } => {
                write!(f, "cannot find module '{specifier}' imported from {from}")
            }
            ResolveError::InvalidSpecifier { specifier } => {
                write!(f, "invalid module specifier '{specifier}'")
            }
            ResolveError::PackagePathNotExported { package, subpath } => {
                write!(f, "package '{package}' does not export '{subpath}'")
            }
            ResolveError::PackageImportNotDefined { specifier, package_dir } => {
                write!(f, "import '{specifier}' is not defined in the package at {package_dir}")
            }
            ResolveError::InvalidPackageTarget { package, target } => {
                write!(f, "invalid target '{target}' in package '{package}'")
            }
            ResolveError::InvalidPackageConfig { path, message } => {
                write!(f, "invalid package configuration {path}: {message}")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<ResolveError> for String {
    fn from(e: ResolveError) -> String {
        e.to_string()
    }
}

type Result<T> = std::result::Result<T, ResolveError>;

pub struct Resolver<F: FileSystem> {
    fs: F,
    options: Options,
}

impl<F: FileSystem> Resolver<F> {
    pub fn new(fs: F, options: Options) -> Resolver<F> {
        Resolver { fs, options }
    }

    /// Resolve `specifier` as imported by the file `referrer`. Returns the
    /// path of the file to load.
    pub fn resolve(&self, specifier: &str, referrer: &str) -> Result<String> {
        let referrer = normalize(referrer);
        self.resolve_from_dir(specifier, dirname(&referrer))
    }

    /// Resolve `specifier` as imported from code living in directory `dir`
    /// (for a root load with no referrer file).
    pub fn resolve_from_dir(&self, specifier: &str, dir: &str) -> Result<String> {
        let dir = normalize(dir);
        let dir = dir.as_str();
        let invalid = || ResolveError::InvalidSpecifier { specifier: specifier.to_string() };
        let not_found = || ResolveError::NotFound { specifier: specifier.to_string(), from: dir.to_string() };
        if specifier.is_empty() {
            return Err(invalid());
        }
        if specifier == "."
            || specifier == ".."
            || specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier.starts_with('/')
            || is_drive_path(specifier)
        {
            let path = join(dir, specifier);
            return self.load_path(&path, specifier.ends_with('/')).ok_or_else(not_found);
        }
        if let Some(rest) = specifier.strip_prefix('#') {
            if rest.is_empty() || rest.starts_with('/') {
                return Err(invalid());
            }
            return self.resolve_import(specifier, dir);
        }
        self.resolve_bare(specifier, dir)
    }

    // ---- files and directories ----

    fn load_path(&self, path: &str, dir_only: bool) -> Option<String> {
        if !dir_only && self.fs.is_file(path) {
            return Some(path.to_string());
        }
        if self.options.extensions.is_empty() {
            return None;
        }
        if !dir_only {
            if let Some(p) = self.with_extension(path) {
                return Some(p);
            }
        }
        if self.fs.is_dir(path) {
            return self.load_dir(path);
        }
        None
    }

    fn with_extension(&self, path: &str) -> Option<String> {
        self.options
            .extensions
            .iter()
            .map(|e| format!("{path}{e}"))
            .find(|p| self.fs.is_file(p))
    }

    fn load_index(&self, dir: &str) -> Option<String> {
        self.with_extension(&format!("{dir}/index"))
    }

    /// `main` of the directory's `package.json`, as a file or a directory
    /// with an index, then the directory's own index.
    fn load_dir(&self, dir: &str) -> Option<String> {
        if let Ok(Some(pj)) = self.read_package(dir) {
            if let Some(Json::Str(main)) = pj.get("main") {
                if !main.is_empty() {
                    let m = join(dir, main);
                    if self.fs.is_file(&m) {
                        return Some(m);
                    }
                    if let Some(p) = self.with_extension(&m).or_else(|| self.load_index(&m)) {
                        return Some(p);
                    }
                }
            }
        }
        self.load_index(dir)
    }

    fn read_package(&self, dir: &str) -> Result<Option<Json>> {
        let path = format!("{dir}/package.json");
        if !self.fs.is_file(&path) {
            return Ok(None);
        }
        let text = self.fs.read_to_string(&path).ok_or_else(|| ResolveError::InvalidPackageConfig {
            path: path.clone(),
            message: "cannot read".into(),
        })?;
        match serde_json::from_str::<Json>(&text) {
            Ok(j @ Json::Obj(_)) => Ok(Some(j)),
            Ok(_) => Err(ResolveError::InvalidPackageConfig { path, message: "not an object".into() }),
            Err(e) => Err(ResolveError::InvalidPackageConfig { path, message: e.to_string() }),
        }
    }

    /// The nearest `package.json` at or above `dir`, not looking inside or
    /// above a `node_modules` directory.
    fn package_scope(&self, dir: &str) -> Result<Option<(String, Json)>> {
        let mut d = dir.to_string();
        loop {
            if basename(&d) == "node_modules" {
                return Ok(None);
            }
            if let Some(pj) = self.read_package(&d)? {
                return Ok(Some((d, pj)));
            }
            match parent(&d) {
                Some(p) => d = p,
                None => return Ok(None),
            }
        }
    }

    // ---- bare specifiers ----

    fn resolve_bare(&self, specifier: &str, dir: &str) -> Result<String> {
        let invalid = || ResolveError::InvalidSpecifier { specifier: specifier.to_string() };
        let (name, subpath) = parse_package_name(specifier).ok_or_else(invalid)?;
        let not_found = || ResolveError::NotFound { specifier: specifier.to_string(), from: dir.to_string() };

        // Self-reference through the enclosing package's own exports.
        if let Some((pdir, pj)) = self.package_scope(dir)? {
            if let (Some(Json::Str(n)), Some(exports)) = (pj.get("name"), pj.get("exports")) {
                if n == name && !matches!(exports, Json::Null) {
                    return self.resolve_exports(&pdir, name, &subpath, exports);
                }
            }
        }

        let mut d = dir.to_string();
        loop {
            if basename(&d) != "node_modules" {
                let pdir = format!("{}/node_modules/{name}", d.trim_end_matches('/'));
                if self.fs.is_dir(&pdir) {
                    let pj = self.read_package(&pdir)?;
                    if let Some(exports) = pj.as_ref().and_then(|p| p.get("exports")) {
                        if !matches!(exports, Json::Null) {
                            return self.resolve_exports(&pdir, name, &subpath, exports);
                        }
                    }
                    return self.legacy_package(&pdir, &subpath).ok_or_else(not_found);
                }
            }
            match parent(&d) {
                Some(p) => d = p,
                None => return Err(not_found()),
            }
        }
    }

    /// A package with no `exports`: `main` for the root, plain paths inside.
    fn legacy_package(&self, pdir: &str, subpath: &str) -> Option<String> {
        if subpath == "." {
            if self.options.extensions.is_empty() {
                let pj = self.read_package(pdir).ok()??;
                let Json::Str(main) = pj.get("main")? else { return None };
                let m = join(pdir, main);
                return self.fs.is_file(&m).then_some(m);
            }
            return self.load_dir(pdir);
        }
        self.load_path(&join(pdir, subpath), false)
    }

    // ---- exports and imports ----

    fn resolve_exports(&self, pdir: &str, name: &str, subpath: &str, exports: &Json) -> Result<String> {
        let not_exported = || ResolveError::PackagePathNotExported {
            package: name.to_string(),
            subpath: subpath.to_string(),
        };
        let dot_keys = match exports {
            Json::Obj(entries) => {
                let dots = entries.iter().filter(|(k, _)| k.starts_with('.')).count();
                if dots != 0 && dots != entries.len() {
                    return Err(ResolveError::InvalidPackageConfig {
                        path: format!("{pdir}/package.json"),
                        message: "\"exports\" mixes subpath and condition keys".into(),
                    });
                }
                dots != 0
            }
            _ => false,
        };
        let sugar;
        let entries: &[(String, Json)] = match exports {
            Json::Obj(e) if dot_keys => e,
            other => {
                sugar = [(".".to_string(), other.clone())];
                &sugar
            }
        };
        let (key_idx, pattern) = match_key(entries, subpath).ok_or_else(not_exported)?;
        match self.resolve_target(pdir, name, &entries[key_idx].1, pattern.as_deref(), false)? {
            Target::Found(path) => {
                if self.fs.is_file(&path) {
                    Ok(path)
                } else {
                    Err(ResolveError::NotFound { specifier: format!("{name}{}", &subpath[1..]), from: pdir.to_string() })
                }
            }
            _ => Err(not_exported()),
        }
    }

    fn resolve_import(&self, specifier: &str, dir: &str) -> Result<String> {
        let undefined = |pdir: &str| ResolveError::PackageImportNotDefined {
            specifier: specifier.to_string(),
            package_dir: pdir.to_string(),
        };
        let Some((pdir, pj)) = self.package_scope(dir)? else {
            return Err(undefined(dir));
        };
        let Some(Json::Obj(entries)) = pj.get("imports") else {
            return Err(undefined(&pdir));
        };
        let (idx, pattern) = match_key(entries, specifier).ok_or_else(|| undefined(&pdir))?;
        match self.resolve_target(&pdir, &pdir, &entries[idx].1, pattern.as_deref(), true)? {
            Target::Found(path) => {
                if self.fs.is_file(&path) {
                    Ok(path)
                } else {
                    Err(ResolveError::NotFound { specifier: specifier.to_string(), from: pdir })
                }
            }
            Target::Bare(spec) => self.resolve_bare(&spec, &pdir),
            _ => Err(undefined(&pdir)),
        }
    }

    fn resolve_target(
        &self,
        pdir: &str,
        pkg: &str,
        target: &Json,
        pattern: Option<&str>,
        internal: bool,
    ) -> Result<Target> {
        let invalid = |t: String| ResolveError::InvalidPackageTarget { package: pkg.to_string(), target: t };
        match target {
            Json::Str(s) => {
                let substituted = match pattern {
                    Some(p) => s.replace('*', p),
                    None => s.clone(),
                };
                if let Some(rest) = substituted.strip_prefix("./") {
                    let bad = rest.split('/').any(|seg| {
                        seg.is_empty() || seg == "." || seg == ".." || seg.eq_ignore_ascii_case("node_modules")
                    });
                    if bad {
                        return Err(invalid(s.clone()));
                    }
                    return Ok(Target::Found(join(pdir, &substituted)));
                }
                let bare_ok = internal
                    && !substituted.starts_with("../")
                    && !substituted.starts_with('/')
                    && !substituted.starts_with('.')
                    && !substituted.contains(':');
                if bare_ok {
                    Ok(Target::Bare(substituted))
                } else {
                    Err(invalid(s.clone()))
                }
            }
            Json::Obj(entries) => {
                for (k, v) in entries {
                    if k == "default" || self.options.conditions.iter().any(|c| c == k) {
                        match self.resolve_target(pdir, pkg, v, pattern, internal)? {
                            Target::Undefined => continue,
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Target::Undefined)
            }
            Json::Arr(items) => {
                let mut last_err = None;
                for item in items {
                    match self.resolve_target(pdir, pkg, item, pattern, internal) {
                        Ok(Target::Undefined) => continue,
                        Ok(other) => return Ok(other),
                        Err(e @ ResolveError::InvalidPackageTarget { .. }) => last_err = Some(e),
                        Err(e) => return Err(e),
                    }
                }
                match last_err {
                    Some(e) => Err(e),
                    None => Ok(Target::Undefined),
                }
            }
            Json::Null => Ok(Target::Blocked),
            _ => Err(invalid(format!("{target:?}"))),
        }
    }
}

enum Target {
    Found(String),
    /// An `imports` target naming another package.
    Bare(String),
    /// An explicit `null`.
    Blocked,
    /// No condition matched.
    Undefined,
}

/// The entry for `key`: an exact match, else the best one-`*` pattern (the
/// longest prefix, then the longest key), with what the `*` stood for.
fn match_key(entries: &[(String, Json)], key: &str) -> Option<(usize, Option<String>)> {
    if let Some(i) = entries.iter().position(|(k, _)| k == key && !k.contains('*')) {
        return Some((i, None));
    }
    let mut best: Option<(usize, &str, &str)> = None;
    for (i, (k, _)) in entries.iter().enumerate() {
        let Some(star) = k.find('*') else { continue };
        if k[star + 1..].contains('*') {
            continue;
        }
        let (prefix, suffix) = (&k[..star], &k[star + 1..]);
        if key.len() >= k.len() && key.starts_with(prefix) && key.ends_with(suffix) {
            let better = match best {
                None => true,
                Some((_, bp, _)) => prefix.len() > bp.len() || (prefix.len() == bp.len() && k.len() > entries[best.unwrap().0].0.len()),
            };
            if better {
                best = Some((i, prefix, suffix));
            }
        }
    }
    best.map(|(i, p, s)| (i, Some(key[p.len()..key.len() - s.len()].to_string())))
}

/// Split `name/sub/path` (or `@scope/name/sub/path`) into the package name
/// and a subpath that starts with `.`.
fn parse_package_name(spec: &str) -> Option<(&str, String)> {
    let name_end = if spec.starts_with('@') {
        let first = spec.find('/')?;
        if first == 1 {
            return None;
        }
        match spec[first + 1..].find('/') {
            Some(i) => first + 1 + i,
            None => spec.len(),
        }
    } else {
        spec.find('/').unwrap_or(spec.len())
    };
    let name = &spec[..name_end];
    let scoped_bad = name.starts_with('@') && name.ends_with('/');
    if name.is_empty() || name.starts_with('.') || name.contains('\\') || name.contains('%') || scoped_bad {
        return None;
    }
    if name.starts_with('@') && name.len() <= name.find('/')? + 1 {
        return None;
    }
    let rest = &spec[name_end..];
    if rest.split('/').skip(1).any(|s| s == "." || s == "..") {
        return None;
    }
    Some((name, format!(".{rest}")))
}

// ---- path strings ----

fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

fn split_root(p: &str) -> (&str, &str) {
    if let Some(rest) = p.strip_prefix('/') {
        ("/", rest)
    } else if is_drive_path(p) {
        (&p[..3], &p[3..])
    } else {
        ("", p)
    }
}

/// Backslashes to slashes, `.` and `..` segments folded, no trailing slash
/// (except on a bare root).
fn normalize(p: &str) -> String {
    let p = p.replace('\\', "/");
    let (root, rest) = split_root(&p);
    let mut segs: Vec<&str> = Vec::new();
    for s in rest.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    format!("{root}{}", segs.join("/"))
}

fn join(dir: &str, rel: &str) -> String {
    let rel = rel.replace('\\', "/");
    if rel.starts_with('/') || is_drive_path(&rel) {
        normalize(&rel)
    } else {
        normalize(&format!("{dir}/{rel}"))
    }
}

fn parent(p: &str) -> Option<String> {
    let (root, rest) = split_root(p);
    if rest.is_empty() {
        return None;
    }
    Some(match rest.rfind('/') {
        Some(i) => format!("{root}{}", &rest[..i]),
        None => root.to_string(),
    })
}

fn dirname(p: &str) -> &str {
    let (root, rest) = split_root(p);
    match rest.rfind('/') {
        Some(i) => &p[..root.len() + i],
        None => root,
    }
}

fn basename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or("")
}

// ---- package.json with key order ----

/// JSON with objects as ordered entry lists: condition matching follows the
/// package author's key order, which `serde_json::Value` does not keep
/// without a workspace-wide feature.
#[derive(Clone, Debug)]
pub(crate) enum Json {
    Null,
    Bool,
    Num,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(e) => e.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Json, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_unit<E>(self) -> std::result::Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, _: bool) -> std::result::Result<Json, E> {
                Ok(Json::Bool)
            }
            fn visit_i64<E>(self, _: i64) -> std::result::Result<Json, E> {
                Ok(Json::Num)
            }
            fn visit_u64<E>(self, _: u64) -> std::result::Result<Json, E> {
                Ok(Json::Num)
            }
            fn visit_f64<E>(self, _: f64) -> std::result::Result<Json, E> {
                Ok(Json::Num)
            }
            fn visit_str<E>(self, s: &str) -> std::result::Result<Json, E> {
                Ok(Json::Str(s.to_string()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(Json::Arr(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some((k, x)) = a.next_entry::<String, Json>()? {
                    v.push((k, x));
                }
                Ok(Json::Obj(v))
            }
        }
        d.deserialize_any(V)
    }
}
