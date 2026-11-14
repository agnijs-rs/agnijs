//! The command's [`Host`]: modules from the file system through the
//! Node-style resolver, host source maps, console to stdout and stderr,
//! unhandled-rejection tracking and the interrupt flag Ctrl-C sets.

use agnijs::api::{
    ConsoleLevel, Engine, Host, ImportAttribute, ModuleSource, PropertyKey, RealmId, RegexMatcher,
    RejectionEvent, Value,
};
use agnijs_js_resolve::{Options, Resolver, StdFs};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How the host was configured from the command line.
pub struct HostConfig {
    pub regex_matcher: RegexMatcher,
    pub seed: Option<u64>,
    pub interrupt: Arc<AtomicBool>,
}

pub struct CliHost {
    resolver: Resolver<StdFs>,
    /// The working directory, with `/` separators: where specifiers of a
    /// root load and of code that is not a file (stdin, `eval`) resolve.
    cwd: String,
    /// Modules that are not files: `[stdin]`, `[eval]`, the REPL.
    virtual_modules: RefCell<HashMap<String, ModuleSource>>,
    /// Rejected promises with no handler yet, oldest first.
    rejections: RefCell<Vec<(Value, Value)>>,
    config: HostConfig,
}

impl CliHost {
    pub fn new(config: HostConfig) -> CliHost {
        let cwd = std::env::current_dir()
            .map(|p| slash_path(&p))
            .unwrap_or_else(|_| ".".into());
        CliHost {
            resolver: Resolver::new(StdFs, Options::default()),
            cwd,
            virtual_modules: RefCell::new(HashMap::new()),
            rejections: RefCell::new(Vec::new()),
            config,
        }
    }

    /// Serve `source` as the module `name` (which is not a file).
    pub fn add_virtual_module(&self, name: &str, source: ModuleSource) {
        self.virtual_modules
            .borrow_mut()
            .insert(name.to_string(), source);
    }

    /// Take the rejections still unhandled.
    pub fn take_rejections(&self) -> Vec<(Value, Value)> {
        std::mem::take(&mut *self.rejections.borrow_mut())
    }

    /// Whether Ctrl-C was pressed since the last call; clears the flag.
    pub fn take_interrupt(&self) -> bool {
        self.config.interrupt.swap(false, Ordering::SeqCst)
    }
}

/// A path as the resolver and stack traces show it: `/` separators.
pub fn slash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// An absolute `/`-separated path for a file named on the command line.
pub fn absolute(file: &str) -> String {
    let p = Path::new(file);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|d| d.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    };
    let s = slash_path(&abs);
    // Drop `.` and resolve `..` the way the resolver does.
    let (root, rest) = match s.find(':') {
        // `C:/` or, for a drive-relative path, just `C:`: never split
        // inside a character that follows the colon (TASK-103.37).
        Some(1) => s.split_at(if s.as_bytes().get(2) == Some(&b'/') {
            3
        } else {
            2
        }),
        _ if s.starts_with('/') => s.split_at(1),
        _ => ("", s.as_str()),
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    format!("{root}{}", segs.join("/"))
}

/// Whether a script or module name is a file path (and not `[eval]`,
/// `REPL1` and the like).
pub fn is_path(name: &str) -> bool {
    let b = name.as_bytes();
    name.starts_with('/') || (b.len() > 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
}

/// `file:` URL of an absolute `/`-separated path.
pub fn file_url(path: &str) -> String {
    let mut out = String::from("file://");
    if !path.starts_with('/') {
        out.push('/');
    }
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The Source Map a source text points at with `//# sourceMappingURL=`:
/// an inline `data:` URL, or a file next to `path`.
pub fn source_map_for(path: &str, source: &str) -> Option<String> {
    let url = agnijs_js_debugger::source_mapping_url(source)?;
    if let Some(data) = url.strip_prefix("data:") {
        let (meta, body) = data.split_once(',')?;
        return if meta.ends_with(";base64") {
            let bytes = agnijs_js_debugger::source_map::base64_decode(body)?;
            String::from_utf8(bytes).ok()
        } else {
            Some(percent_decode(body))
        };
    }
    if url.contains("://") {
        return None;
    }
    let dir = Path::new(path).parent()?;
    std::fs::read_to_string(dir.join(percent_decode(&url))).ok()
}

// The debugger's decoder works on bytes; the copy this file had sliced the
// string at byte offsets and panicked on a multi-byte character after `%`
// (TASK-103.37).
use agnijs_js_debugger::source_map::percent_decode;

/// Whether `bytes` are a precompiled container (`agnijs compile`).
pub fn is_bytecode(bytes: &[u8]) -> bool {
    bytes.starts_with(agnijs::api::bytecode::MAGIC)
}

impl Host for CliHost {
    fn resolve_module(
        &self,
        referrer: Option<&str>,
        specifier: &str,
        _attributes: &[ImportAttribute],
    ) -> Result<String, String> {
        let virtuals = self.virtual_modules.borrow();
        if virtuals.contains_key(specifier) {
            return Ok(specifier.to_string());
        }
        if specifier.starts_with("node:") {
            return Err(format!(
                "cannot find module '{specifier}': agnijs has no Node.js built-in modules"
            ));
        }
        match referrer {
            Some(r) if !virtuals.contains_key(r) && is_path(r) => {
                self.resolver.resolve(specifier, r)
            }
            _ => self.resolver.resolve_from_dir(specifier, &self.cwd),
        }
        .map_err(|e| e.to_string())
    }

    fn load_module(&self, resolved: &str) -> Result<ModuleSource, String> {
        if let Some(src) = self.virtual_modules.borrow().get(resolved) {
            return Ok(src.clone());
        }
        let bytes = std::fs::read(resolved).map_err(|e| format!("cannot read {resolved}: {e}"))?;
        if is_bytecode(&bytes) {
            return Ok(ModuleSource::Bytecode(bytes));
        }
        let source = String::from_utf8(bytes).map_err(|_| format!("{resolved} is not UTF-8"))?;
        if resolved.ends_with(".json") {
            return Ok(ModuleSource::Json(source));
        }
        Ok(match source_map_for(resolved, &source) {
            Some(source_map) => ModuleSource::SourceWithMap { source, source_map },
            None => ModuleSource::Source(source),
        })
    }

    fn import_meta(
        &self,
        engine: &mut dyn Engine,
        realm: RealmId,
        module: &str,
        meta: &Value,
    ) -> agnijs::api::Result<()> {
        if self.virtual_modules.borrow().contains_key(module) {
            let url = file_url(&format!("{}/{module}", self.cwd));
            return engine.set(realm, meta, &PropertyKey::from("url"), &Value::string(&url));
        }
        let path = Path::new(module);
        let dir = path.parent().map(slash_path).unwrap_or_default();
        engine.set(
            realm,
            meta,
            &PropertyKey::from("url"),
            &Value::string(&file_url(module)),
        )?;
        engine.set(
            realm,
            meta,
            &PropertyKey::from("filename"),
            &Value::string(module),
        )?;
        engine.set(
            realm,
            meta,
            &PropertyKey::from("dirname"),
            &Value::string(&dir),
        )
    }

    fn promise_rejection(&self, engine: &mut dyn Engine, _realm: RealmId, event: RejectionEvent) {
        match event {
            RejectionEvent::Reject { promise, reason } => {
                self.rejections.borrow_mut().push((promise, reason))
            }
            RejectionEvent::Handle { promise } => {
                let mut r = self.rejections.borrow_mut();
                if let Some(i) = r
                    .iter()
                    .position(|(p, _)| engine.strict_equals(p, &promise))
                {
                    r.remove(i);
                }
            }
        }
    }

    fn console(&self, _realm: RealmId, level: ConsoleLevel, args: Vec<String>) {
        let line = args.join(" ");
        // A closed pipe (`agnijs x.js | head`) is not an error worth a panic.
        let _ = match level {
            ConsoleLevel::Warn | ConsoleLevel::Error => writeln!(std::io::stderr(), "{line}"),
            _ => writeln!(std::io::stdout(), "{line}"),
        };
    }

    fn interrupt(&self) -> bool {
        self.config.interrupt.load(Ordering::SeqCst)
    }

    fn random_seed(&self) -> u64 {
        match self.config.seed {
            Some(s) => s,
            None => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0);
                nanos ^ (std::process::id() as u64).rotate_left(32)
            }
        }
    }

    fn regex_matcher(&self) -> RegexMatcher {
        self.config.regex_matcher
    }
}

#[cfg(test)]
mod tests {
    use super::{absolute, percent_decode, source_map_for};

    /// A character of more than one byte after `%` is not an escape, and
    /// stepping over it must not panic (TASK-103.37).
    #[test]
    fn percent_decoding_steps_over_multi_byte_characters() {
        for (input, want) in [
            ("%a\u{e9}1", "%a\u{e9}1"),
            ("%\u{e9}", "%\u{e9}"),
            ("%\u{e9}1", "%\u{e9}1"),
            ("x%\u{1F600}", "x%\u{1F600}"),
            ("%", "%"),
            ("%4", "%4"),
            ("%41", "A"),
            ("ab%41", "abA"),
            ("%41%42", "AB"),
            ("%zz%41", "%zzA"),
            ("%C3%A9", "\u{e9}"),
        ] {
            let got = std::panic::catch_unwind(|| percent_decode(input));
            assert_eq!(got.as_deref().ok(), Some(want), "{input:?}");
        }
    }

    #[test]
    fn a_data_url_with_a_bad_escape_is_read_without_a_panic() {
        let src = "1\n//# sourceMappingURL=data:,%a\u{e9}1";
        let got = std::panic::catch_unwind(|| source_map_for("x.js", src));
        assert_eq!(got.ok(), Some(Some("%a\u{e9}1".to_string())));
        // An escape in the last three bytes is decoded.
        let src = "1\n//# sourceMappingURL=data:application/json,%7B%7D%41";
        assert_eq!(source_map_for("x.js", src).as_deref(), Some("{}A"));
    }

    /// A drive letter followed by a multi-byte character (`C:é.js`, a
    /// drive-relative path on Windows) used to be split inside the
    /// character.
    #[test]
    fn absolute_paths_split_their_root_on_a_character_boundary() {
        for p in ["C:é.js", "C:é", "C:", "C:/é.js", "/é.js"] {
            assert!(std::panic::catch_unwind(|| absolute(p)).is_ok(), "{p:?}");
        }
    }
}
