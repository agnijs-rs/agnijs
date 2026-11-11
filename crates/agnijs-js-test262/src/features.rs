//! The ES2025 feature cut.
//!
//! `features.txt` has three sections (proposed, standard, test-harness) and
//! no edition field. The cut is: every feature in the standard section at
//! the pinned commit, minus the checked-in list of features that reached
//! Stage 4 after the ES2025 (16th edition) cut. Proposed features are
//! excluded; harness features are allowed (tests that need `IsHTMLDDA` are
//! excluded through the exclusions file instead).

use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct FeatureList {
    pub proposed: BTreeSet<String>,
    pub standard: BTreeSet<String>,
    pub harness: BTreeSet<String>,
}

impl FeatureList {
    pub fn parse(text: &str) -> FeatureList {
        let mut list = FeatureList::default();
        let mut section = 0;
        for raw in text.lines() {
            let line = raw.trim();
            if line.starts_with("## Proposed") {
                section = 1;
                continue;
            }
            if line.starts_with("## Standard") {
                section = 2;
                continue;
            }
            if line.starts_with("## Test-Harness") {
                section = 3;
                continue;
            }
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let name = line.split('#').next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                continue;
            }
            match section {
                1 => list.proposed.insert(name),
                2 => list.standard.insert(name),
                3 => list.harness.insert(name),
                _ => false,
            };
        }
        list
    }
}

/// Parse the checked-in post-ES2025 list (one feature per line, `#` comments).
pub fn parse_name_list(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The set of features a test may use and still count toward the ES2025 gate.
pub struct FeatureCut {
    allowed: BTreeSet<String>,
    /// Feature names from the post-2025 list that are not in `features.txt`
    /// (the pin moved and the list is stale).
    pub unknown_post_2025: Vec<String>,
}

impl FeatureCut {
    pub fn new(list: &FeatureList, post_2025: &BTreeSet<String>) -> FeatureCut {
        let mut allowed: BTreeSet<String> = list.standard.difference(post_2025).cloned().collect();
        allowed.extend(list.harness.iter().cloned());
        let unknown_post_2025 = post_2025
            .iter()
            .filter(|f| !list.standard.contains(*f) && !list.proposed.contains(*f))
            .cloned()
            .collect();
        FeatureCut {
            allowed,
            unknown_post_2025,
        }
    }

    /// `Err(feature)` names the first feature outside the cut.
    pub fn check<'a>(&self, features: &'a [String]) -> Result<(), &'a str> {
        for f in features {
            if !self.allowed.contains(f) {
                return Err(f);
            }
        }
        Ok(())
    }

    /// Admit one more feature (`test262 run --allow-feature`).
    pub fn allow(&mut self, feature: &str) {
        self.allowed.insert(feature.to_string());
    }

    pub fn allows(&self, feature: &str) -> bool {
        self.allowed.contains(feature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sections_and_strips_comments() {
        let text = "## Proposed language features\n# c\ndecorators\n## Standard language features\nclass\nalign-x  # https://x\n## Test-Harness Features\nIsHTMLDDA\n";
        let l = FeatureList::parse(text);
        assert!(l.proposed.contains("decorators"));
        assert!(l.standard.contains("class") && l.standard.contains("align-x"));
        assert!(l.harness.contains("IsHTMLDDA"));
        let cut = FeatureCut::new(&l, &parse_name_list("align-x\nbogus\n"));
        assert!(cut.allows("class") && !cut.allows("align-x") && !cut.allows("decorators"));
        assert!(cut.allows("IsHTMLDDA"));
        assert_eq!(cut.unknown_post_2025, vec!["bogus".to_string()]);
    }
}
