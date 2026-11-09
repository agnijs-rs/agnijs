//! The exclusions file: one rule per line, `<selector> | <reason>`.
//!
//! Selectors:
//! - `test/built-ins/Atomics/wait/` (a directory prefix, trailing slash)
//! - `test/language/foo/bar.js` (one file)
//! - `feature:IsHTMLDDA` (any test listing the feature)
//! - `flag:CanBlockIsFalse` (any test with the flag)
//! - `include:atomicsHelper.js` (any test including the harness file)
//!
//! A rule without a reason is an error: the runner refuses to start.

use crate::metadata::Metadata;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    Dir(String),
    File(String),
    Feature(String),
    Flag(String),
    Include(String),
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selector: Selector,
    pub reason: String,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct Exclusions {
    pub rules: Vec<Rule>,
}

impl Exclusions {
    pub fn parse(text: &str) -> Result<Exclusions, String> {
        let mut rules = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((selector, reason)) = line.split_once('|') else {
                return Err(format!(
                    "exclusions line {}: missing `| reason` in {line:?}",
                    i + 1
                ));
            };
            let selector = selector.trim();
            let reason = reason.trim();
            if reason.is_empty() {
                return Err(format!(
                    "exclusions line {}: empty reason for {selector:?}",
                    i + 1
                ));
            }
            if selector.is_empty() {
                return Err(format!("exclusions line {}: empty selector", i + 1));
            }
            let selector = if let Some(f) = selector.strip_prefix("feature:") {
                Selector::Feature(f.trim().to_string())
            } else if let Some(f) = selector.strip_prefix("flag:") {
                Selector::Flag(f.trim().to_string())
            } else if let Some(f) = selector.strip_prefix("include:") {
                Selector::Include(f.trim().to_string())
            } else if selector.ends_with('/') {
                Selector::Dir(selector.replace('\\', "/"))
            } else {
                Selector::File(selector.replace('\\', "/"))
            };
            rules.push(Rule {
                selector,
                reason: reason.to_string(),
                line: i + 1,
            });
        }
        Ok(Exclusions { rules })
    }

    /// The reason the test at `path` (relative to the test262 root, with
    /// forward slashes) is excluded, if any rule matches.
    pub fn reason_for(&self, path: &str, meta: &Metadata) -> Option<&str> {
        for rule in &self.rules {
            let hit = match &rule.selector {
                Selector::Dir(d) => path.starts_with(d.as_str()),
                Selector::File(f) => path == f,
                Selector::Feature(f) => meta.features.iter().any(|x| x == f),
                Selector::Flag(f) => meta.flags.iter().any(|x| x == f),
                Selector::Include(f) => meta.includes.iter().any(|x| x == f),
            };
            if hit {
                return Some(&rule.reason);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_reason() {
        assert!(Exclusions::parse("test/x.js").is_err());
        assert!(Exclusions::parse("test/x.js |   ").is_err());
        assert!(Exclusions::parse("# comment\n\nfeature:IsHTMLDDA | out of scope\n").is_ok());
    }

    #[test]
    fn matches_selectors() {
        let ex = Exclusions::parse(
            "test/built-ins/Atomics/wait/ | multi-agent\nfeature:IsHTMLDDA | html dda\nflag:CanBlockIsFalse | no agents\ninclude:atomicsHelper.js | agents\ntest/a/b.js | one file\n",
        )
        .unwrap();
        let mut m = Metadata::default();
        assert_eq!(
            ex.reason_for("test/built-ins/Atomics/wait/x.js", &m),
            Some("multi-agent")
        );
        assert_eq!(
            ex.reason_for("test/built-ins/Atomics/notify/x.js", &m),
            None
        );
        assert_eq!(ex.reason_for("test/a/b.js", &m), Some("one file"));
        m.features.push("IsHTMLDDA".into());
        assert_eq!(ex.reason_for("test/z.js", &m), Some("html dda"));
        m.features.clear();
        m.includes.push("atomicsHelper.js".into());
        assert_eq!(ex.reason_for("test/z.js", &m), Some("agents"));
    }
}
