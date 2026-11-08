//! `Intl.ListFormat` on ICU4X's list formatter.

use icu_list::options::{ListFormatterOptions, ListLength};
use icu_list::{ListFormatter, ListFormatterPreferences};
use icu_locale_core::Locale;
use writeable::{PartsWrite, Writeable};

use crate::number::Part;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListType {
    Conjunction,
    Disjunction,
    Unit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyle {
    Long,
    Short,
    Narrow,
}

pub struct IntlListFormatter {
    formatter: ListFormatter,
}

impl IntlListFormatter {
    pub fn new(tag: &str, ty: ListType, style: ListStyle) -> Result<IntlListFormatter, String> {
        let locale: Locale = tag.parse().map_err(|_| format!("bad locale {tag}"))?;
        let prefs = ListFormatterPreferences::from(&locale);
        let len = match style {
            ListStyle::Long => ListLength::Wide,
            ListStyle::Short => ListLength::Short,
            ListStyle::Narrow => ListLength::Narrow,
        };
        let opts = ListFormatterOptions::default().with_length(len);
        let formatter = match ty {
            ListType::Conjunction => ListFormatter::try_new_and(prefs, opts),
            ListType::Disjunction => ListFormatter::try_new_or(prefs, opts),
            ListType::Unit => ListFormatter::try_new_unit(prefs, opts),
        }
        .map_err(|e| e.to_string())?;
        Ok(IntlListFormatter { formatter })
    }

    pub fn format_to_parts(&self, items: &[String]) -> Vec<Part> {
        let formatted = self.formatter.format(items.iter());
        let mut sink = Sink { parts: Vec::new(), depth: 0, element: false };
        let _ = formatted.write_to_parts(&mut sink);
        sink.parts
    }

    pub fn format(&self, items: &[String]) -> String {
        self.formatter.format(items.iter()).write_to_string().into_owned()
    }
}

struct Sink {
    parts: Vec<Part>,
    depth: usize,
    element: bool,
}

impl Sink {
    fn push(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let kind = if self.element { "element" } else { "literal" };
        if let Some(last) = self.parts.last_mut() {
            if last.kind == kind {
                last.value.push_str(s);
                return;
            }
        }
        self.parts.push(Part { kind, value: s.to_string() });
    }
}

impl std::fmt::Write for Sink {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.push(s);
        Ok(())
    }
    fn write_char(&mut self, c: char) -> std::fmt::Result {
        let mut b = [0u8; 4];
        self.push(c.encode_utf8(&mut b));
        Ok(())
    }
}

impl PartsWrite for Sink {
    type SubPartsWrite = Sink;
    fn with_part(&mut self, part: writeable::Part, f: impl FnOnce(&mut Sink) -> std::fmt::Result) -> std::fmt::Result {
        let saved = self.element;
        if self.depth == 0 {
            self.element = part == icu_list::parts::ELEMENT;
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        self.element = saved;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn english_lists() {
        let f = IntlListFormatter::new("en", ListType::Conjunction, ListStyle::Long).unwrap();
        assert_eq!(f.format(&s(&["a", "b", "c"])), "a, b, and c");
        assert_eq!(f.format(&s(&["a", "b"])), "a and b");
        assert_eq!(f.format(&s(&["a"])), "a");
        assert_eq!(f.format(&s(&[])), "");
        let parts = f.format_to_parts(&s(&["a", "b"]));
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].kind, "element");
        assert_eq!(parts[1].kind, "literal");
        assert_eq!(parts[1].value, " and ");
        let o = IntlListFormatter::new("en", ListType::Disjunction, ListStyle::Short).unwrap();
        assert_eq!(o.format(&s(&["a", "b", "c"])), "a, b, or c");
        let u = IntlListFormatter::new("en", ListType::Unit, ListStyle::Narrow).unwrap();
        assert_eq!(u.format(&s(&["a", "b", "c"])), "a b c");
        let de = IntlListFormatter::new("de", ListType::Conjunction, ListStyle::Long).unwrap();
        assert_eq!(de.format(&s(&["a", "b", "c"])), "a, b und c");
    }
}
