//! The test262 runner for engines behind the AgniJS engine interface.
//!
//! Layout:
//! - [`metadata`]: frontmatter parsing
//! - [`features`]: the ES2025 feature cut
//! - [`exclusions`]: the exclusions file (every rule carries a reason)
//! - [`runner`]: one test, per INTERPRETING.md
//! - [`suite`]: discovery, parallel runs, report, baseline comparison
//!
//! The pinned test262 commit lives in `PIN` next to the crate; `test262 fetch`
//! checks it out into `test262/`.

pub mod exclusions;
pub mod features;
pub mod metadata;
pub mod runner;
pub mod suite;

/// The pinned test262 commit, read from `PIN` at build time.
pub const PINNED_COMMIT: &str = include_str!("../PIN");

pub fn pinned_commit() -> &'static str {
    PINNED_COMMIT.trim()
}
