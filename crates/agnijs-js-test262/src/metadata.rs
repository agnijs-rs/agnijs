//! test262 frontmatter (`/*--- ... ---*/`) parsing.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Negative {
    pub phase: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Metadata {
    #[serde(default)]
    pub includes: Vec<String>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub negative: Option<Negative>,
    #[serde(default)]
    pub locale: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub esid: Option<String>,
    #[serde(default)]
    pub es5id: Option<String>,
    #[serde(default)]
    pub es6id: Option<String>,
    #[serde(default)]
    pub info: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub defines: Vec<String>,
}

impl Metadata {
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
    pub fn is_module(&self) -> bool {
        self.has_flag("module")
    }
    pub fn is_raw(&self) -> bool {
        self.has_flag("raw")
    }
    pub fn is_async(&self) -> bool {
        self.has_flag("async")
    }
}

/// Extract and parse the frontmatter block. A file without one yields the
/// default metadata (harness files are like that).
pub fn parse(source: &str) -> Result<Metadata, String> {
    let Some(start) = source.find("/*---") else {
        return Ok(Metadata::default());
    };
    let rest = &source[start + 5..];
    let Some(end) = rest.find("---*/") else {
        return Err("unterminated frontmatter".into());
    };
    let yaml = &rest[..end];
    // Some files indent the whole block; YAML needs a consistent indent, so
    // strip the common leading whitespace.
    let yaml = dedent(yaml);
    serde_yaml::from_str::<Metadata>(&yaml).map_err(|e| format!("frontmatter: {e}"))
}

fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_block() {
        let src = "// Copyright\n/*---\nesid: sec-x\ndescription: hello\nincludes: [compareArray.js]\nflags: [onlyStrict, async]\nfeatures: [Symbol, class]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\ncode";
        let m = parse(src).unwrap();
        assert_eq!(m.includes, vec!["compareArray.js"]);
        assert!(m.has_flag("onlyStrict") && m.is_async());
        assert_eq!(m.features, vec!["Symbol", "class"]);
        let n = m.negative.unwrap();
        assert_eq!(
            (n.phase.as_str(), n.kind.as_str()),
            ("parse", "SyntaxError")
        );
    }

    #[test]
    fn no_frontmatter_is_default() {
        assert!(parse("var x = 1;").unwrap().flags.is_empty());
    }
}
