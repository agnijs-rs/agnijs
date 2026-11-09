//! The ICU4X seam for the AgniJS JavaScript engine.
//!
//! Everything locale-sensitive the ECMA-402 built-ins need lives here in
//! engine-independent terms (strings, `f64`, decimal digit strings, part
//! lists). `agnijs-js-vm` implements the ECMA-402 abstract operations and
//! object shapes and calls into this crate; nothing in here knows about JS
//! values, so the ICU4X version can move without touching the VM.

pub(crate) mod provider;
pub mod tables;
pub mod decimal;
pub mod locale;
pub mod timezone;
pub mod cldr_tables;
pub mod data;
pub mod number;
pub mod plural;
pub mod datetime;
pub mod collator;
pub mod list;
pub mod relativetime;
pub mod segmenter;
pub mod displaynames;
pub mod casemap;
