//! WHATWG import maps (HTML, "Import maps"; ADR 0025).
//!
//! An import map remaps module specifiers before the host loads them:
//! `"imports"` applies everywhere, `"scopes"` apply to modules whose URL
//! starts with the scope prefix, and keys ending in `/` remap whole
//! packages. An embedder parses the map once with [`ImportMap::parse`] and
//! calls [`ImportMap::resolve`] from its `Host::resolve_module`, with the
//! referrer's URL as the base (or the document or entry URL for a root
//! load). Results are serialized absolute URLs.
//!
//! The algorithms follow the HTML standard: parsing ("parse an import map
//! string", "sort and normalize a module specifier map", "sort and
//! normalize scopes", "normalize a module integrity map") and resolution
//! ("resolve a module specifier", "resolve an imports match"). URL parsing
//! is the WHATWG URL Standard, through the `url` crate. Not included: the
//! merging of several import maps into one (a browser concern, since a
//! document can hold several `<script type="importmap">` elements) and
//! fetching itself.

use crate::Json;
use std::collections::HashMap;
use std::fmt;
use url::Url;

/// Why an import map did not parse or a specifier did not resolve. In a
/// browser each of these is a `TypeError`, except `InvalidJson`, which is a
/// `SyntaxError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportMapError {
    /// The import map text is not JSON.
    InvalidJson { message: String },
    /// JSON of the wrong shape: the top level, `imports`, `scopes`,
    /// `integrity` or one scope is not an object.
    InvalidShape { message: String },
    /// A base URL that is not an absolute URL.
    InvalidBaseUrl { url: String },
    /// A bare specifier that no entry maps.
    Unmapped { specifier: String, base_url: String },
    /// The matching entry has no valid address (its value was not a string,
    /// not a URL, or lacked the trailing `/` its key has), which blocks the
    /// specifier.
    Blocked { specifier: String, key: String },
    /// A package (trailing-`/`) entry matched, but the rest of the
    /// specifier does not resolve to a URL under the entry's address.
    EscapesPackage { specifier: String, key: String },
}

impl fmt::Display for ImportMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportMapError::InvalidJson { message } => write!(f, "import map is not valid JSON: {message}"),
            ImportMapError::InvalidShape { message } => write!(f, "invalid import map: {message}"),
            ImportMapError::InvalidBaseUrl { url } => write!(f, "'{url}' is not an absolute URL"),
            ImportMapError::Unmapped { specifier, base_url } => write!(
                f,
                "bare specifier '{specifier}' imported from {base_url} is not mapped by the import map"
            ),
            ImportMapError::Blocked { specifier, key } => {
                write!(f, "specifier '{specifier}' is blocked by the import map entry '{key}', which has no valid address")
            }
            ImportMapError::EscapesPackage { specifier, key } => write!(
                f,
                "specifier '{specifier}' does not resolve to a URL inside the address of the import map entry '{key}'"
            ),
        }
    }
}

impl std::error::Error for ImportMapError {}

impl From<ImportMapError> for String {
    fn from(e: ImportMapError) -> String {
        e.to_string()
    }
}

/// Normalized specifier keys to addresses (`None`: an invalid address that
/// blocks the key), in descending code-unit order of the keys.
type SpecifierMap = Vec<(String, Option<Url>)>;

/// A parsed, normalized import map.
///
/// ```
/// use agnijs_js_resolve::ImportMap;
///
/// let map = ImportMap::parse(
///     r#"{ "imports": { "lodash": "/vendor/lodash.js", "app/": "./src/" },
///          "scopes": { "/vendor/": { "lodash": "/vendor/lodash-v3.js" } } }"#,
///     "https://example.com/index.html",
/// )
/// .unwrap();
/// let entry = "https://example.com/index.html";
/// assert_eq!(map.resolve("lodash", entry).unwrap(), "https://example.com/vendor/lodash.js");
/// assert_eq!(map.resolve("app/main.js", entry).unwrap(), "https://example.com/src/main.js");
/// // Inside the /vendor/ scope, its own entry wins.
/// let vendored = "https://example.com/vendor/plugin.js";
/// assert_eq!(map.resolve("lodash", vendored).unwrap(), "https://example.com/vendor/lodash-v3.js");
/// assert!(map.resolve("react", entry).is_err());
/// ```
#[derive(Clone, Debug, Default)]
pub struct ImportMap {
    imports: SpecifierMap,
    /// Normalized scope prefixes, in descending code-unit order.
    scopes: Vec<(String, SpecifierMap)>,
    integrity: HashMap<String, String>,
    warnings: Vec<String>,
}

impl ImportMap {
    /// Parse an import map's JSON text. Relative addresses, URL-like keys
    /// and scope prefixes resolve against `base_url` (in a browser, the URL
    /// of the document or of the import map's script).
    pub fn parse(text: &str, base_url: &str) -> Result<ImportMap, ImportMapError> {
        let base = parse_base(base_url)?;
        let json: Json = serde_json::from_str(text).map_err(|e| ImportMapError::InvalidJson {
            message: e.to_string(),
        })?;
        let Json::Obj(top) = &json else {
            return Err(shape("the top level must be a JSON object"));
        };
        let top = js_object(top);
        let mut map = ImportMap::default();
        if let Some(v) = get(&top, "imports") {
            let Json::Obj(entries) = v else {
                return Err(shape("\"imports\" must be a JSON object"));
            };
            map.imports = normalize_specifier_map(entries, &base, &mut map.warnings);
        }
        if let Some(v) = get(&top, "scopes") {
            let Json::Obj(entries) = v else {
                return Err(shape("\"scopes\" must be a JSON object"));
            };
            map.scopes = normalize_scopes(entries, &base, &mut map.warnings)?;
        }
        if let Some(v) = get(&top, "integrity") {
            let Json::Obj(entries) = v else {
                return Err(shape("\"integrity\" must be a JSON object"));
            };
            map.integrity = normalize_integrity(entries, &base, &mut map.warnings);
        }
        for (key, _) in &top {
            if !matches!(*key, "imports" | "scopes" | "integrity") {
                map.warnings
                    .push(format!("unknown top-level key '{key}' is ignored"));
            }
        }
        Ok(map)
    }

    /// Resolve `specifier` as imported by the module at `base_url` (for a
    /// root load, the document or entry URL). Returns the serialized URL to
    /// load. A relative or absolute URL that no entry remaps resolves as a
    /// URL; a bare specifier that no entry maps is an error.
    pub fn resolve(&self, specifier: &str, base_url: &str) -> Result<String, ImportMapError> {
        let base = parse_base(base_url)?;
        let as_url = resolve_url_like(specifier, &base);
        let normalized = as_url.as_ref().map_or(specifier, Url::as_str);
        let base_str = base.as_str();
        for (prefix, imports) in &self.scopes {
            if prefix == base_str
                || (prefix.ends_with('/') && base_str.starts_with(prefix.as_str()))
            {
                if let Some(url) = resolve_imports_match(normalized, as_url.as_ref(), imports)? {
                    return Ok(url);
                }
            }
        }
        if let Some(url) = resolve_imports_match(normalized, as_url.as_ref(), &self.imports)? {
            return Ok(url);
        }
        match as_url {
            Some(url) => Ok(url.into()),
            None => Err(ImportMapError::Unmapped {
                specifier: specifier.to_string(),
                base_url: base_str.to_string(),
            }),
        }
    }

    /// The integrity metadata the map gives for a resolved URL ("resolve a
    /// module integrity metadata"), if any. The host checks it when it
    /// fetches; the map only records it.
    pub fn integrity(&self, url: &str) -> Option<&str> {
        self.integrity.get(url).map(String::as_str)
    }

    /// The normalized top-level `imports`, in the order resolution tries
    /// them. `None` is an entry with no valid address.
    pub fn imports(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        entries(&self.imports)
    }

    /// The normalized scopes, in the order resolution tries them, each with
    /// its normalized specifier map.
    pub fn scopes(
        &self,
    ) -> impl Iterator<Item = (&str, impl Iterator<Item = (&str, Option<&str>)>)> {
        self.scopes
            .iter()
            .map(|(prefix, map)| (prefix.as_str(), entries(map)))
    }

    /// The non-fatal problems parsing found (the console warnings a browser
    /// reports): ignored keys and entries with no valid address.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// True for a map with no imports, scopes or integrity entries.
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty() && self.scopes.is_empty() && self.integrity.is_empty()
    }
}

fn entries(map: &SpecifierMap) -> impl Iterator<Item = (&str, Option<&str>)> {
    map.iter()
        .map(|(k, v)| (k.as_str(), v.as_ref().map(Url::as_str)))
}

fn shape(message: &str) -> ImportMapError {
    ImportMapError::InvalidShape {
        message: message.to_string(),
    }
}

fn parse_base(base_url: &str) -> Result<Url, ImportMapError> {
    Url::parse(base_url).map_err(|_| ImportMapError::InvalidBaseUrl {
        url: base_url.to_string(),
    })
}

/// "Resolve a URL-like module specifier": `/`, `./` and `../` specifiers
/// resolve against the base; anything else counts only if it is an
/// absolute URL on its own.
fn resolve_url_like(specifier: &str, base: &Url) -> Option<Url> {
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return base.join(specifier).ok();
    }
    Url::parse(specifier).ok()
}

fn is_special(url: &Url) -> bool {
    matches!(
        url.scheme(),
        "ftp" | "file" | "http" | "https" | "ws" | "wss"
    )
}

/// "Resolve an imports match": the exact key first, else the longest
/// trailing-`/` key that prefixes the specifier (descending order puts both
/// first). `Ok(None)`: no entry applies.
fn resolve_imports_match(
    normalized: &str,
    as_url: Option<&Url>,
    map: &SpecifierMap,
) -> Result<Option<String>, ImportMapError> {
    for (key, address) in map {
        let blocked = || ImportMapError::Blocked {
            specifier: normalized.to_string(),
            key: key.clone(),
        };
        if key == normalized {
            return match address {
                Some(url) => Ok(Some(url.as_str().to_string())),
                None => Err(blocked()),
            };
        }
        if key.ends_with('/')
            && normalized.starts_with(key.as_str())
            && as_url.is_none_or(is_special)
        {
            let Some(address) = address else {
                return Err(blocked());
            };
            let escapes = || ImportMapError::EscapesPackage {
                specifier: normalized.to_string(),
                key: key.clone(),
            };
            let after_prefix = &normalized[key.len()..];
            let url = address.join(after_prefix).map_err(|_| escapes())?;
            if !url.as_str().starts_with(address.as_str()) {
                return Err(escapes());
            }
            return Ok(Some(url.into()));
        }
    }
    Ok(None)
}

/// "Sort and normalize a module specifier map".
fn normalize_specifier_map(
    entries: &[(String, Json)],
    base: &Url,
    warnings: &mut Vec<String>,
) -> SpecifierMap {
    let mut out = OrderedMap::default();
    for (key, value) in js_object(entries) {
        // "Normalize a specifier key".
        if key.is_empty() {
            warnings.push("an empty specifier key is ignored".to_string());
            continue;
        }
        let normalized_key = match resolve_url_like(key, base) {
            Some(url) => String::from(url),
            None => key.to_string(),
        };
        let Json::Str(address) = value else {
            warnings.push(format!(
                "the address of '{key}' is not a string; the entry blocks the specifier"
            ));
            out.set(normalized_key, None);
            continue;
        };
        let Some(address_url) = resolve_url_like(address, base) else {
            warnings.push(format!(
                "the address '{address}' of '{key}' is not a URL; the entry blocks the specifier"
            ));
            out.set(normalized_key, None);
            continue;
        };
        if key.ends_with('/') && !address_url.as_str().ends_with('/') {
            warnings.push(format!(
                "the address '{address}' of the package key '{key}' does not end in '/'; the entry blocks the specifier"
            ));
            out.set(normalized_key, None);
            continue;
        }
        out.set(normalized_key, Some(address_url));
    }
    out.sorted_descending()
}

/// "Sort and normalize scopes".
fn normalize_scopes(
    entries: &[(String, Json)],
    base: &Url,
    warnings: &mut Vec<String>,
) -> Result<Vec<(String, SpecifierMap)>, ImportMapError> {
    let mut out = OrderedMap::default();
    for (prefix, value) in js_object(entries) {
        let Json::Obj(map) = value else {
            return Err(shape(&format!(
                "the scope '{prefix}' must be a JSON object"
            )));
        };
        let Ok(prefix_url) = base.join(prefix) else {
            warnings.push(format!(
                "the scope prefix '{prefix}' is not a URL; the scope is ignored"
            ));
            continue;
        };
        out.set(
            prefix_url.into(),
            normalize_specifier_map(map, base, warnings),
        );
    }
    Ok(out.sorted_descending())
}

/// "Normalize a module integrity map".
fn normalize_integrity(
    entries: &[(String, Json)],
    base: &Url,
    warnings: &mut Vec<String>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, value) in js_object(entries) {
        let Some(url) = resolve_url_like(key, base) else {
            warnings.push(format!(
                "the integrity key '{key}' is not a URL; the entry is ignored"
            ));
            continue;
        };
        let Json::Str(metadata) = value else {
            warnings.push(format!(
                "the integrity metadata of '{key}' is not a string; the entry is ignored"
            ));
            continue;
        };
        out.insert(String::from(url), metadata.clone());
    }
    out
}

/// An insertion-ordered map with "set" semantics (a repeated key keeps its
/// place and takes the new value), sorted at the end.
struct OrderedMap<V> {
    entries: Vec<(String, V)>,
    index: HashMap<String, usize>,
}

impl<V> Default for OrderedMap<V> {
    fn default() -> Self {
        OrderedMap {
            entries: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<V> OrderedMap<V> {
    fn set(&mut self, key: String, value: V) {
        match self.index.get(&key) {
            Some(&i) => self.entries[i].1 = value,
            None => {
                self.index.insert(key.clone(), self.entries.len());
                self.entries.push((key, value));
            }
        }
    }

    /// Descending code-unit (UTF-16) order of the keys, as the standard
    /// sorts: longer keys before their prefixes.
    fn sorted_descending(mut self) -> Vec<(String, V)> {
        self.entries
            .sort_by(|a, b| b.0.encode_utf16().cmp(a.0.encode_utf16()));
        self.entries
    }
}

fn get<'a>(object: &[(&str, &'a Json)], key: &str) -> Option<&'a Json> {
    object.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// An object's entries in the order `JSON.parse` and then property
/// enumeration give them, which the standard's "parse a JSON string to an
/// Infra value" goes through: a repeated key keeps its first place and its
/// last value, and array-index keys come first in ascending numeric order.
fn js_object(entries: &[(String, Json)]) -> Vec<(&str, &Json)> {
    let mut order: Vec<&str> = Vec::new();
    let mut values: HashMap<&str, &Json> = HashMap::new();
    for (k, v) in entries {
        if values.insert(k.as_str(), v).is_none() {
            order.push(k.as_str());
        }
    }
    let mut indices: Vec<(u32, &str)> = order
        .iter()
        .filter_map(|k| array_index(k).map(|i| (i, *k)))
        .collect();
    indices.sort_unstable_by_key(|(i, _)| *i);
    let mut out = Vec::with_capacity(order.len());
    for (_, k) in &indices {
        out.push((*k, values[k]));
    }
    for k in &order {
        if array_index(k).is_none() {
            out.push((*k, values[k]));
        }
    }
    out
}

/// A canonical array index: `0`, or digits without a leading zero, below
/// 2^32 - 1.
fn array_index(key: &str) -> Option<u32> {
    if key.is_empty()
        || !key.bytes().all(|b| b.is_ascii_digit())
        || (key.len() > 1 && key.starts_with('0'))
    {
        return None;
    }
    key.parse::<u32>().ok().filter(|&i| i != u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_object_order_puts_indices_first_and_keeps_the_last_value() {
        let json: Json =
            serde_json::from_str(r#"{"b":1,"10":2,"a":3,"2":4,"b":"x","01":5}"#).unwrap();
        let Json::Obj(e) = &json else { panic!() };
        let keys: Vec<&str> = js_object(e).iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["2", "10", "b", "a", "01"]);
        assert!(matches!(get(&js_object(e), "b"), Some(Json::Str(s)) if s == "x"));
    }

    /// The web-platform-tests data-driven import map tests
    /// (`import-maps/data-driven/resources/*.json` in a WPT checkout), when
    /// `AGNIJS_WPT_IMPORT_MAPS` names that directory; skipped otherwise. The
    /// files are walked with the crate's key-ordered JSON so each map's
    /// text keeps the key order the test wrote.
    #[test]
    fn wpt_data_driven_import_maps() {
        let Ok(dir) = std::env::var("AGNIJS_WPT_IMPORT_MAPS") else {
            eprintln!("AGNIJS_WPT_IMPORT_MAPS not set; skipping the WPT import map tests");
            return;
        };
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        let (mut passed, mut failures) = (0usize, Vec::new());
        for path in &files {
            let text = std::fs::read_to_string(path).unwrap();
            let json: Json = serde_json::from_str(&text).unwrap();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            wpt_node(&json, &name, &Ctx::default(), &mut passed, &mut failures);
        }
        for f in &failures {
            eprintln!("FAIL {f}");
        }
        eprintln!(
            "WPT import maps: {passed} passed, {} failed, {} files",
            failures.len(),
            files.len()
        );
        assert!(
            failures.is_empty(),
            "{} WPT import map checks failed",
            failures.len()
        );
    }

    #[derive(Clone, Default)]
    struct Ctx<'a> {
        import_map: Option<&'a Json>,
        import_map_base: Option<&'a str>,
        base: Option<&'a str>,
        expected_parsed: Option<&'a Json>,
        expected_results: Option<&'a Json>,
    }

    fn wpt_node<'a>(
        node: &'a Json,
        path: &str,
        parent: &Ctx<'a>,
        passed: &mut usize,
        failures: &mut Vec<String>,
    ) {
        let Json::Obj(fields) = node else { return };
        let field = |k: &str| fields.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let s = |v: Option<&'a Json>| match v {
            Some(Json::Str(s)) => Some(s.as_str()),
            _ => None,
        };
        let ctx = Ctx {
            import_map: field("importMap").or(parent.import_map),
            import_map_base: s(field("importMapBaseURL")).or(parent.import_map_base),
            base: s(field("baseURL")).or(parent.base),
            expected_parsed: field("expectedParsedImportMap").or(parent.expected_parsed),
            expected_results: field("expectedResults").or(parent.expected_results),
        };
        if let Some(Json::Obj(tests)) = field("tests") {
            for (name, child) in tests {
                wpt_node(child, &format!("{path} > {name}"), &ctx, passed, failures);
            }
            return;
        }
        let parsed = ctx
            .import_map
            .zip(ctx.import_map_base)
            .map(|(m, b)| ImportMap::parse(&to_text(m), b));
        if let Some(expected) = ctx.expected_parsed {
            match (expected, &parsed) {
                (Json::Null, Some(Err(_))) => *passed += 1,
                (Json::Null, _) => failures.push(format!("{path}: parsed but should throw")),
                (_, Some(Ok(map))) => {
                    if parsed_as_json(map) == plain(expected) {
                        *passed += 1;
                    } else {
                        failures.push(format!(
                            "{path}: parsed {:?}, expected {:?}",
                            parsed_as_json(map),
                            plain(expected)
                        ));
                    }
                }
                (_, other) => failures.push(format!("{path}: should parse, got {other:?}")),
            }
        }
        if let Some(Json::Obj(results)) = ctx.expected_results {
            match &parsed {
                Some(Ok(map)) => {
                    let base = ctx.base.unwrap_or_default();
                    for (spec, want) in results {
                        let got = map.resolve(spec, base).ok();
                        let want = s(Some(want));
                        if got.as_deref() == want {
                            *passed += 1;
                        } else {
                            failures.push(format!("{path}: resolve '{spec}' from {base}: got {got:?}, expected {want:?}"));
                        }
                    }
                }
                other => failures.push(format!("{path}: no map to resolve with ({other:?})")),
            }
        }
    }

    /// JSON text with the key order kept. Numbers and booleans are never
    /// valid import map values, so one of each stands for all (0, true).
    fn to_text(j: &Json) -> String {
        match j {
            Json::Null => "null".into(),
            Json::Bool => "true".into(),
            Json::Num => "0".into(),
            Json::Str(s) => serde_json::to_string(s).unwrap(),
            Json::Arr(a) => format!("[{}]", a.iter().map(to_text).collect::<Vec<_>>().join(",")),
            Json::Obj(o) => format!(
                "{{{}}}",
                o.iter()
                    .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), to_text(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }

    fn plain(j: &Json) -> serde_json::Value {
        serde_json::from_str(&to_text(j)).unwrap()
    }

    fn parsed_as_json(map: &ImportMap) -> serde_json::Value {
        let specifier_map = |it: &mut dyn Iterator<Item = (&str, Option<&str>)>| {
            serde_json::Value::Object(
                it.map(|(k, v)| {
                    (
                        k.to_string(),
                        v.map_or(serde_json::Value::Null, |v| v.into()),
                    )
                })
                .collect(),
            )
        };
        let mut out = serde_json::Map::new();
        out.insert("imports".into(), specifier_map(&mut map.imports()));
        let scopes: serde_json::Map<String, serde_json::Value> = map
            .scopes()
            .map(|(p, mut m)| (p.to_string(), specifier_map(&mut m)))
            .collect();
        out.insert("scopes".into(), scopes.into());
        if !map.integrity.is_empty() {
            let integrity: serde_json::Map<String, serde_json::Value> = map
                .integrity
                .iter()
                .map(|(k, v)| (k.clone(), v.clone().into()))
                .collect();
            out.insert("integrity".into(), integrity.into());
        }
        serde_json::Value::Object(out)
    }

    #[test]
    fn array_index_bounds() {
        assert_eq!(array_index("0"), Some(0));
        assert_eq!(array_index("4294967294"), Some(4294967294));
        assert_eq!(array_index("4294967295"), None);
        assert_eq!(array_index("00"), None);
        assert_eq!(array_index("-1"), None);
    }
}
