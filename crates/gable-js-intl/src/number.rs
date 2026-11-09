//! `Intl.NumberFormat` services: ECMA-402 rounding (through
//! [`crate::decimal`]), digit rendering with CLDR symbols, and the
//! decimal/percent/currency/unit styles with standard, scientific,
//! engineering and compact notations. The VM resolves options and hands
//! this module a [`NumberFormatOptions`]; the result is a list of parts.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use icu_locale_core::Locale;
use icu_pattern::{DoublePlaceholderKey, PatternItem, SinglePlaceholderKey};
use icu_plurals::{PluralCategory, PluralOperands, PluralRules, PluralRulesPreferences};
use icu_provider::prelude::*;

use crate::cldr_tables::{NumberMisc, UnitLocale, NUMBER_MISC, UNIT_LOCALES};
use crate::decimal::{to_raw_fixed, to_raw_precision, Dec, RoundingMode};
use crate::provider::IntlData;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrencyDisplay {
    Code,
    Symbol,
    NarrowSymbol,
    Name,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CurrencySign {
    Standard,
    Accounting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitDisplay {
    Short,
    Narrow,
    Long,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Style {
    Decimal,
    Percent,
    Currency { code: String, display: CurrencyDisplay, sign: CurrencySign },
    Unit { unit: String, display: UnitDisplay },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notation {
    Standard,
    Scientific,
    Engineering,
    Compact { long: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    Always,
    Auto,
    Min2,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignDisplay {
    Auto,
    Never,
    Always,
    ExceptZero,
    Negative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundingType {
    FractionDigits,
    SignificantDigits,
    MorePrecision,
    LessPrecision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrailingZeroDisplay {
    Auto,
    StripIfInteger,
}

/// The digit options after SetNumberFormatDigitOptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digits {
    pub min_integer: usize,
    pub min_fraction: usize,
    pub max_fraction: usize,
    pub min_significant: usize,
    pub max_significant: usize,
    pub rounding_type: RoundingType,
    pub rounding_mode: RoundingMode,
    pub rounding_increment: u32,
    pub trailing_zero_display: TrailingZeroDisplay,
}

impl Digits {
    /// Plain decimal defaults (0..3 fraction digits).
    pub fn decimal() -> Digits {
        Digits {
            min_integer: 1,
            min_fraction: 0,
            max_fraction: 3,
            min_significant: 1,
            max_significant: 21,
            rounding_type: RoundingType::FractionDigits,
            rounding_mode: RoundingMode::HalfExpand,
            rounding_increment: 1,
            trailing_zero_display: TrailingZeroDisplay::Auto,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberFormatOptions {
    pub locale: String,
    pub numbering_system: String,
    pub style: Style,
    pub notation: Notation,
    pub grouping: Grouping,
    pub sign_display: SignDisplay,
    pub digits: Digits,
}

/// A value to format: the result of ToIntlMathematicalValue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Num {
    Nan,
    PosInf,
    NegInf,
    Finite(Dec),
}

impl Num {
    pub fn from_f64(x: f64) -> Num {
        if x.is_nan() {
            Num::Nan
        } else if x == f64::INFINITY {
            Num::PosInf
        } else if x == f64::NEG_INFINITY {
            Num::NegInf
        } else {
            Num::Finite(Dec::from_f64(x))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub kind: &'static str,
    pub value: String,
}

fn part(kind: &'static str, value: impl Into<String>) -> Part {
    Part { kind, value: value.into() }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangePart {
    pub kind: &'static str,
    pub value: String,
    pub source: &'static str,
}

#[derive(Clone, Debug)]
struct Symbols {
    minus_prefix: String,
    minus_suffix: String,
    plus_prefix: String,
    plus_suffix: String,
    decimal: String,
    group: String,
    primary: u8,
    secondary: u8,
    min_grouping: u8,
}

type CompactTuple = zerovec::ule::vartuple::VarTupleULE<
    u8,
    icu_plurals::provider::PluralElementsPackedULE<icu_pattern::Pattern<icu_pattern::SinglePlaceholder>>,
>;

pub(crate) fn load<M>(locale: &Locale, attrs: &str) -> Option<DataPayload<M>>
where
    M: DataMarker,
    IntlData: DataProvider<M>,
{
    let attrs = DataMarkerAttributes::try_from_str(attrs).ok()?;
    let dl = DataLocale::from(locale);
    let req = DataRequest {
        id: DataIdentifierBorrowed::for_marker_attributes_and_locale(attrs, &dl),
        metadata: Default::default(),
    };
    DataProvider::<M>::load(&IntlData, req).ok().map(|r| r.payload)
}

fn load_symbols(locale: &Locale, nu: &str) -> Option<DataPayload<icu_decimal::provider::DecimalSymbolsV1>> {
    load(locale, nu)
        .or_else(|| {
            // CLDR root carries symbols for the Arabic numbering systems
            // (`٫`, `٬`); ICU4X only stores them under the locales that
            // use them, so `en-u-nu-arab` borrows the Arabic entry.
            let root: Option<&str> = match nu {
                "arab" => Some("ar"),
                "arabext" => Some("fa"),
                _ => None,
            };
            let root: Locale = root?.parse().ok()?;
            load(&root, nu)
        })
        .or_else(|| load(locale, ""))
}

/// The decimal separator of a locale in a numbering system (`""` for the
/// locale default).
pub fn decimal_separator(tag: &str, nu: &str) -> String {
    let base = crate::locale::base_name(tag);
    let Ok(locale) = base.parse::<Locale>() else { return ".".to_string() };
    load_symbols(&locale, nu).map(|p| p.get().decimal_separator().to_string()).unwrap_or_else(|| ".".to_string())
}

/// The locale's default numbering system (the `nu` of its decimal symbols).
pub fn default_numbering_system(tag: &str) -> String {
    let base = crate::locale::base_name(tag);
    let locale: Locale = match base.parse() {
        Ok(l) => l,
        Err(_) => return "latn".to_string(),
    };
    load::<icu_decimal::provider::DecimalSymbolsV1>(&locale, "")
        .map(|p| p.get().numsys().to_string())
        .unwrap_or_else(|| "latn".to_string())
}

/// Every ISO 4217 code CLDR has a display name for.
pub fn known_currencies() -> Vec<String> {
    use icu_experimental::dimension::provider::currency::displayname::CurrencyDisplaynameV1;
    let mut set = BTreeSet::new();
    if let Ok(ids) = IterableDataProvider::<CurrencyDisplaynameV1>::iter_ids(&IntlData) {
        for id in ids {
            let a = id.marker_attributes.as_str();
            if a.len() == 3 && a.bytes().all(|b| b.is_ascii_uppercase()) {
                set.insert(a.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// The long display name of a currency in a locale, from CLDR currency data.
pub fn currency_display_name(locale: &Locale, code: &str) -> Option<String> {
    use icu_experimental::dimension::provider::currency::displayname::CurrencyDisplaynameV1;
    let p = load::<CurrencyDisplaynameV1>(locale, code)?;
    Some(p.get().to_string())
}

/// CurrencyDigits: the minor-unit digits of an ISO 4217 code (2 when
/// unknown).
pub fn currency_digits(code: &str) -> u32 {
    use icu_experimental::dimension::provider::currency::fractions::CurrencyFractionsV1;
    let und: Locale = Locale::UNKNOWN;
    let Some(p) = load::<CurrencyFractionsV1>(&und, "") else { return 2 };
    let f = p.get();
    let key = match tinystr::TinyAsciiStr::<3>::try_from_str(&code.to_ascii_uppercase()) {
        Ok(k) => k.to_unvalidated(),
        Err(_) => return f.default.digits as u32,
    };
    f.fractions
        .get(&key)
        .map(|i| <icu_experimental::dimension::provider::currency::fractions::FractionInfo as zerovec::ule::AsULE>::from_unaligned(*i).digits as u32)
        .unwrap_or(f.default.digits as u32)
}

/// Candidate keys for the bundled CLDR tables: the tag, `lang-REGION`,
/// `lang-Script`, `lang`, then `en`.
pub(crate) fn table_candidates(tag: &str) -> Vec<String> {
    let base = crate::locale::base_name(tag);
    let mut v = vec![base.clone()];
    let max = crate::locale::maximize(&base);
    let p = crate::locale::parts(&max);
    if let Some(r) = &p.region {
        v.push(format!("{}-{}", p.language, r));
    }
    if let Some(s) = &p.script {
        v.push(format!("{}-{}", p.language, s));
    }
    v.push(p.language.clone());
    v.push("en".to_string());
    v
}

fn unit_locale(tag: &str) -> &'static UnitLocale {
    for c in table_candidates(tag) {
        if let Some((_, u)) = UNIT_LOCALES.iter().find(|(l, _)| *l == c) {
            return u;
        }
    }
    &UNIT_LOCALES[0].1
}

fn number_misc(tag: &str, nu: &str) -> &'static NumberMisc {
    for c in table_candidates(tag) {
        if let Some((_, systems)) = NUMBER_MISC.iter().find(|(l, _)| *l == c) {
            if let Some((_, m)) = systems.iter().find(|(n, _)| *n == nu) {
                return m;
            }
            if let Some((_, m)) = systems.iter().find(|(n, _)| *n == "latn") {
                return m;
            }
        }
    }
    &NUMBER_MISC[0].1[0].1
}

/// Whether the bundled unit tables have an entry for the locale itself
/// (not just the `en` fallback).
pub fn has_unit_data(tag: &str) -> bool {
    let c = table_candidates(tag);
    c[..c.len() - 1].iter().any(|c| UNIT_LOCALES.iter().any(|(l, _)| l == c))
}

pub struct NumberFormatter {
    opts: NumberFormatOptions,
    symbols: Symbols,
    digits: Vec<char>,
    misc: &'static NumberMisc,
    rules: PluralRules,
    compact: Option<DataPayload<icu_decimal::provider::DecimalCompactShortV1>>,
    compact_long: Option<DataPayload<icu_decimal::provider::DecimalCompactLongV1>>,
    percent: Option<DataPayload<icu_experimental::dimension::provider::percent::DecimalPercentV1>>,
    currency: Option<CurrencyData>,
    unit: Option<UnitData>,
}

struct CurrencyData {
    essentials: DataPayload<icu_experimental::dimension::provider::currency::essentials::CurrencyEssentialsV1>,
    symbol: String,
    starts_with_letter: bool,
    ends_with_letter: bool,
    names: Option<DataPayload<icu_experimental::dimension::provider::currency::extended::CurrencyExtendedDataV1>>,
    unit_patterns: Option<DataPayload<icu_experimental::dimension::provider::currency::patterns::CurrencyPatternsDataV1>>,
}

struct UnitData {
    /// (count, pattern) per plural category; `{0}` marks the number.
    patterns: Vec<(String, String)>,
}

/// The rounded value and the exponent its notation removed.
pub struct Rounded {
    pub value: Dec,
    pub exponent: i32,
}

impl NumberFormatter {
    pub fn new(opts: NumberFormatOptions) -> Result<NumberFormatter, String> {
        let base = crate::locale::base_name(&opts.locale);
        let locale: Locale = base.parse().map_err(|_| format!("bad locale {}", opts.locale))?;
        let nu = opts.numbering_system.clone();
        let sym = load_symbols(&locale, &nu).ok_or_else(|| "no decimal symbols".to_string())?;
        let s = sym.get();
        let symbols = Symbols {
            minus_prefix: s.minus_sign_prefix().to_string(),
            minus_suffix: s.minus_sign_suffix().to_string(),
            plus_prefix: s.plus_sign_prefix().to_string(),
            plus_suffix: s.plus_sign_suffix().to_string(),
            decimal: s.decimal_separator().to_string(),
            group: s.grouping_separator().to_string(),
            primary: s.grouping_sizes.primary,
            secondary: s.grouping_sizes.secondary,
            min_grouping: s.grouping_sizes.min_grouping,
        };
        let digits = crate::data::digits_of(&nu).unwrap_or_else(|| "0123456789".chars().collect());
        let misc = number_misc(&opts.locale, &nu);
        let prefs = PluralRulesPreferences::from(&locale);
        let rules = PluralRules::try_new_cardinal(prefs).map_err(|e| e.to_string())?;
        let (compact, compact_long) = match opts.notation {
            Notation::Compact { long: false } => (load(&locale, ""), None),
            Notation::Compact { long: true } => (None, load(&locale, "")),
            _ => (None, None),
        };
        let percent = if opts.style == Style::Percent { load(&locale, "") } else { None };
        let currency = match &opts.style {
            Style::Currency { code, display, .. } => Some(CurrencyData::new(&locale, code, display)?),
            _ => None,
        };
        let unit = match &opts.style {
            Style::Unit { unit, display } => Some(UnitData::new(&opts.locale, unit, *display)?),
            _ => None,
        };
        Ok(NumberFormatter { opts, symbols, digits, misc, rules, compact, compact_long, percent, currency, unit })
    }

    pub fn options(&self) -> &NumberFormatOptions {
        &self.opts
    }

    /// FormatNumericToString on a finite value (no notation scaling).
    pub fn round(&self, x: &Dec) -> Dec {
        round_with(&self.opts.digits, x)
    }

    /// The plural category of a finite value after rounding (used by the
    /// currency-name and unit patterns and by `Intl.PluralRules`).
    pub fn plural_category(&self, rounded: &Dec) -> PluralCategory {
        self.rules.category_for(operands(rounded))
    }

    pub fn format_to_parts(&self, x: &Num) -> Vec<Part> {
        self.format_inner(x).0
    }

    pub fn format(&self, x: &Num) -> String {
        self.format_to_parts(x).iter().map(|p| p.value.as_str()).collect()
    }

    /// Format a value, returning the parts and the rounded value.
    pub fn format_inner(&self, x: &Num) -> (Vec<Part>, Option<Rounded>) {
        match x {
            Num::Nan => {
                let body = vec![part("nan", self.misc.nan)];
                (self.compose(body, SignKind::for_value(None, false, self.opts.sign_display), None), None)
            }
            Num::PosInf | Num::NegInf => {
                let neg = *x == Num::NegInf;
                let body = vec![part("infinity", self.misc.infinity)];
                (self.compose(body, SignKind::for_value(Some(neg), false, self.opts.sign_display), None), None)
            }
            Num::Finite(d) => {
                let mut v = d.clone();
                if self.opts.style == Style::Percent {
                    v.shift(2);
                }
                let (body, rounded) = self.notation_parts(&v);
                let is_zero = rounded.value.is_zero();
                let sign = SignKind::for_value(Some(rounded.value.neg), is_zero, self.opts.sign_display);
                let body = self.compose(body, sign, Some(&rounded));
                (body, Some(rounded))
            }
        }
    }

    /// The digits (and exponent or compact affix) of a finite value.
    fn notation_parts(&self, x: &Dec) -> (Vec<Part>, Rounded) {
        match self.opts.notation {
            Notation::Standard => {
                let r = self.round(x);
                (self.digit_parts(&r), Rounded { value: r, exponent: 0 })
            }
            Notation::Scientific | Notation::Engineering => {
                let (r, exp) = self.scaled(x, |m| self.sci_exponent(m));
                let mut parts = self.digit_parts(&r);
                parts.push(part("exponentSeparator", self.misc.exponential));
                if exp < 0 {
                    parts.push(part("exponentMinusSign", self.symbols.minus_prefix.clone()));
                }
                parts.push(part("exponentInteger", self.digit_string(&exp.unsigned_abs().to_string())));
                (parts, Rounded { value: r, exponent: exp })
            }
            Notation::Compact { .. } => {
                let (r, exp) = self.scaled(x, |m| self.compact_exponent(m));
                let num = self.digit_parts(&r);
                let total_mag = r.magnitude() + exp;
                let parts = match self.compact_pattern(total_mag, &r) {
                    Some(pattern) => interpolate_single(&pattern, num, "compact"),
                    None => num,
                };
                (parts, Rounded { value: r, exponent: exp })
            }
        }
    }

    /// ComputeExponent: scale `x` by the exponent its magnitude selects,
    /// round, and re-select if rounding carried into a new digit.
    fn scaled(&self, x: &Dec, exponent_for: impl Fn(i32) -> i32) -> (Dec, i32) {
        if x.is_zero() {
            return (self.round(x), 0);
        }
        let mag = x.magnitude();
        let mut exp = exponent_for(mag);
        let mut v = x.clone();
        v.shift(-exp);
        let mut r = self.round(&v);
        if !r.is_zero() && r.magnitude() + exp > mag {
            let exp2 = exponent_for(mag + 1);
            if exp2 != exp {
                exp = exp2;
                v = x.clone();
                v.shift(-exp);
                r = self.round(&v);
            }
        }
        (r, exp)
    }

    fn sci_exponent(&self, magnitude: i32) -> i32 {
        match self.opts.notation {
            Notation::Engineering => magnitude.div_euclid(3) * 3,
            _ => magnitude,
        }
    }

    fn compact_tuples(&self) -> Option<Vec<&CompactTuple>> {
        if let Some(p) = &self.compact {
            return Some(p.get().0.iter().collect());
        }
        if let Some(p) = &self.compact_long {
            return Some(p.get().0.iter().collect());
        }
        None
    }

    fn compact_exponent(&self, magnitude: i32) -> i32 {
        if magnitude < 0 {
            return 0;
        }
        let m = magnitude.min(i16::MAX as i32) as i16;
        let Some(tuples) = self.compact_tuples() else { return 0 };
        tuples
            .iter()
            .filter(|t| m >= t.sized as i16)
            .last()
            .map(|t| t.sized as i32 - t.variable.get_default().0.get() as i32)
            .unwrap_or(0)
    }

    /// The compact pattern for a value of the given total magnitude, as a
    /// string with `{0}` for the number; `None` when no affix applies.
    fn compact_pattern(&self, total_magnitude: i32, significand: &Dec) -> Option<String> {
        let op = operands(significand);
        let m = total_magnitude.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let tuples = self.compact_tuples()?;
        let t = tuples.iter().filter(|t| m >= t.sized as i16).last()?;
        let pattern = t.variable.get(op, &self.rules).1;
        let s = single_to_string(pattern.iter());
        if !s.contains("{0}") || s == "{0}" {
            return None;
        }
        Some(s)
    }

    fn digit_string(&self, ascii: &str) -> String {
        ascii.chars().map(|c| if c.is_ascii_digit() { self.digits[(c as u8 - b'0') as usize] } else { c }).collect()
    }

    /// integer/group/decimal/fraction parts of a rounded value.
    fn digit_parts(&self, r: &Dec) -> Vec<Part> {
        let plain = r.to_plain(self.opts.digits.min_integer, 0);
        let (int, frac) = match plain.split_once('.') {
            Some((i, f)) => (i.to_string(), Some(f.to_string())),
            None => (plain, None),
        };
        let mut parts = Vec::new();
        let groups = self.group(&int);
        for (i, g) in groups.iter().enumerate() {
            if i > 0 {
                parts.push(part("group", self.symbols.group.clone()));
            }
            parts.push(part("integer", self.digit_string(g)));
        }
        if let Some(f) = frac {
            if !f.is_empty() {
                parts.push(part("decimal", self.symbols.decimal.clone()));
                parts.push(part("fraction", self.digit_string(&f)));
            }
        }
        parts
    }

    fn group(&self, int: &str) -> Vec<String> {
        let len = int.len();
        let primary = self.symbols.primary.max(1) as usize;
        let secondary = if self.symbols.secondary == 0 { primary } else { self.symbols.secondary as usize };
        let min_needed = match self.opts.grouping {
            Grouping::Never => return vec![int.to_string()],
            Grouping::Always => primary + 1,
            Grouping::Auto => primary + self.symbols.min_grouping.max(1) as usize,
            Grouping::Min2 => primary + 2.max(self.symbols.min_grouping as usize),
        };
        if len < min_needed {
            return vec![int.to_string()];
        }
        let mut groups = Vec::new();
        let mut end = len;
        groups.push(int[end - primary..end].to_string());
        end -= primary;
        while end > secondary {
            groups.push(int[end - secondary..end].to_string());
            end -= secondary;
        }
        if end > 0 {
            groups.push(int[..end].to_string());
        }
        groups.reverse();
        groups
    }

    fn sign_parts(&self, sign: SignKind) -> (Vec<Part>, Vec<Part>) {
        match sign {
            SignKind::None => (vec![], vec![]),
            SignKind::Minus => (
                nonempty("minusSign", &self.symbols.minus_prefix),
                nonempty("minusSign", &self.symbols.minus_suffix),
            ),
            SignKind::Plus => (
                nonempty("plusSign", &self.symbols.plus_prefix),
                nonempty("plusSign", &self.symbols.plus_suffix),
            ),
        }
    }

    /// Wrap the number parts in the style's pattern and sign.
    fn compose(&self, number: Vec<Part>, sign: SignKind, rounded: Option<&Rounded>) -> Vec<Part> {
        let (pre, post) = self.sign_parts(sign);
        let signed = |number: Vec<Part>| {
            let mut v = pre.clone();
            v.extend(number);
            v.extend(post.clone());
            v
        };
        match &self.opts.style {
            Style::Decimal => signed(number),
            Style::Percent => {
                let v = signed(number);
                match &self.percent {
                    Some(p) => {
                        let pattern = single_to_string(p.get().unsigned_pattern.iter());
                        interpolate_single(&pattern, v, "percentSign")
                    }
                    None => {
                        let mut v = v;
                        v.push(part("percentSign", self.misc.percent_sign));
                        v
                    }
                }
            }
            Style::Unit { .. } => {
                let unit = self.unit.as_ref().expect("unit data");
                let v = signed(number);
                let cat = rounded.map(|r| self.plural_category(&r.value)).unwrap_or(PluralCategory::Other);
                let pattern = unit.pattern_for(category_name(cat));
                interpolate_single(pattern, v, "unit")
            }
            Style::Currency { display, sign: cur_sign, .. } => {
                let c = self.currency.as_ref().expect("currency data");
                if *display == CurrencyDisplay::Name {
                    return c.name_parts(self, signed(number), rounded);
                }
                let accounting = *cur_sign == CurrencySign::Accounting;
                let ess = c.essentials.get();
                let negative_pattern = if sign == SignKind::Minus {
                    if accounting {
                        ess.get_negative_accounting(c.starts_with_letter, c.ends_with_letter)
                    } else {
                        ess.get_negative(c.starts_with_letter, c.ends_with_letter)
                    }
                } else {
                    None
                };
                let positive = if accounting {
                    ess.get_positive_accounting(c.starts_with_letter, c.ends_with_letter)
                } else {
                    ess.get_positive(c.starts_with_letter, c.ends_with_letter)
                };
                match negative_pattern {
                    Some(p) => {
                        // The pattern carries the sign as a literal.
                        let mut out = Vec::new();
                        let mut number_slot = Some(number);
                        for item in p.iter() {
                            match item {
                                PatternItem::Placeholder(DoublePlaceholderKey::Place0) => {
                                    if let Some(n) = number_slot.take() {
                                        out.extend(n);
                                    }
                                }
                                PatternItem::Placeholder(DoublePlaceholderKey::Place1) => {
                                    out.push(part("currency", c.symbol.clone()));
                                }
                                PatternItem::Literal(l) => {
                                    push_literal_with_sign(&mut out, l, &self.symbols.minus_prefix);
                                }
                            }
                        }
                        out
                    }
                    None => {
                        let mut body = Vec::new();
                        let mut number_slot = Some(number);
                        for item in positive.iter() {
                            match item {
                                PatternItem::Placeholder(DoublePlaceholderKey::Place0) => {
                                    if let Some(n) = number_slot.take() {
                                        body.extend(n);
                                    }
                                }
                                PatternItem::Placeholder(DoublePlaceholderKey::Place1) => {
                                    body.push(part("currency", c.symbol.clone()));
                                }
                                PatternItem::Literal(l) => push_literal(&mut body, l),
                            }
                        }
                        signed(body)
                    }
                }
            }
        }
    }

    /// PartitionNumberRangePattern.
    pub fn format_range_to_parts(&self, a: &Num, b: &Num) -> Vec<RangePart> {
        let (pa, ra) = self.format_inner(a);
        let (pb, rb) = self.format_inner(b);
        let same = match (&ra, &rb) {
            (Some(x), Some(y)) => x.value == y.value && x.exponent == y.exponent,
            _ => a == b,
        };
        if same && pa == pb {
            let mut out = Vec::new();
            for item in split_pattern(self.misc.approximately) {
                match item {
                    Piece::Placeholder(_) => {
                        out.extend(pa.iter().map(|p| RangePart { kind: p.kind, value: p.value.clone(), source: "shared" }));
                    }
                    Piece::Literal(l) => {
                        for (kind, text) in split_affix(l, "approximatelySign") {
                            out.push(RangePart { kind, value: text, source: "shared" });
                        }
                    }
                }
            }
            return out;
        }
        let is_number = |p: &Part| {
            matches!(
                p.kind,
                "integer" | "group" | "decimal" | "fraction" | "nan" | "infinity" | "exponentSeparator" | "exponentMinusSign" | "exponentInteger"
            )
        };
        let first_num = |v: &Vec<Part>| v.iter().position(is_number).unwrap_or(0);
        let last_num = |v: &Vec<Part>| v.iter().rposition(is_number).map(|i| i + 1).unwrap_or(v.len());
        let (fa, la) = (first_num(&pa), last_num(&pa));
        let (fb, lb) = (first_num(&pb), last_num(&pb));
        let prefix_a = &pa[..fa];
        let prefix_b = &pb[..fb];
        let suffix_a = &pa[la..];
        let suffix_b = &pb[lb..];
        let count = |v: &[Part]| v.iter().map(|p| p.value.chars().count()).sum::<usize>();
        let collapse_prefix = prefix_a == prefix_b && count(prefix_a) > 1;
        let collapse_suffix = suffix_a == suffix_b && count(suffix_a) > 1;
        let repeated = (!collapse_prefix && (!prefix_a.is_empty() || !prefix_b.is_empty()))
            || (!collapse_suffix && (!suffix_a.is_empty() || !suffix_b.is_empty()));
        let mut out = Vec::new();
        let push = |out: &mut Vec<RangePart>, parts: &[Part], source: &'static str| {
            out.extend(parts.iter().map(|p| RangePart { kind: p.kind, value: p.value.clone(), source }));
        };
        for piece in split_pattern(self.misc.range) {
            match piece {
                Piece::Placeholder(0) => {
                    if collapse_prefix {
                        push(&mut out, prefix_a, "shared");
                        push(&mut out, &pa[fa..la], "startRange");
                    } else {
                        push(&mut out, &pa[..la], "startRange");
                    }
                    if !collapse_suffix {
                        push(&mut out, suffix_a, "startRange");
                    }
                }
                Piece::Placeholder(_) => {
                    if !collapse_prefix {
                        push(&mut out, prefix_b, "endRange");
                    }
                    push(&mut out, &pb[fb..lb], "endRange");
                    if collapse_suffix {
                        push(&mut out, suffix_b, "shared");
                    } else {
                        push(&mut out, suffix_b, "endRange");
                    }
                }
                Piece::Literal(l) => {
                    let mut text = l.to_string();
                    if repeated {
                        if !text.starts_with(char::is_whitespace) {
                            text.insert(0, ' ');
                        }
                        if !text.ends_with(char::is_whitespace) {
                            text.push(' ');
                        }
                    }
                    out.push(RangePart { kind: "literal", value: text, source: "shared" });
                }
            }
        }
        out
    }

    pub fn format_range(&self, a: &Num, b: &Num) -> String {
        self.format_range_to_parts(a, b).iter().map(|p| p.value.as_str()).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SignKind {
    None,
    Minus,
    Plus,
}

impl SignKind {
    /// GetNumberFormatPattern's sign choice. `neg` is `None` for NaN.
    fn for_value(neg: Option<bool>, is_zero: bool, display: SignDisplay) -> SignKind {
        let negative = neg == Some(true);
        match display {
            SignDisplay::Never => SignKind::None,
            SignDisplay::Auto => {
                if negative {
                    SignKind::Minus
                } else {
                    SignKind::None
                }
            }
            SignDisplay::Always => {
                if negative {
                    SignKind::Minus
                } else {
                    SignKind::Plus
                }
            }
            SignDisplay::ExceptZero => {
                if neg.is_none() || is_zero {
                    SignKind::None
                } else if negative {
                    SignKind::Minus
                } else {
                    SignKind::Plus
                }
            }
            SignDisplay::Negative => {
                if negative && !is_zero {
                    SignKind::Minus
                } else {
                    SignKind::None
                }
            }
        }
    }
}

fn nonempty(kind: &'static str, s: &str) -> Vec<Part> {
    if s.is_empty() {
        vec![]
    } else {
        vec![part(kind, s)]
    }
}

/// FormatNumericToString's rounding, including trailingZeroDisplay.
pub fn round_with(d: &Digits, x: &Dec) -> Dec {
    let mode = d.rounding_mode;
    let r = match d.rounding_type {
        RoundingType::FractionDigits => to_raw_fixed(x, d.min_fraction, d.max_fraction, d.rounding_increment, mode).0,
        RoundingType::SignificantDigits => to_raw_precision(x, d.min_significant, d.max_significant, mode).0,
        RoundingType::MorePrecision | RoundingType::LessPrecision => {
            let (sd, sd_mag) = to_raw_precision(x, d.min_significant, d.max_significant, mode);
            let (fd, fd_mag) = to_raw_fixed(x, d.min_fraction, d.max_fraction, d.rounding_increment, mode);
            let pick_sd = match d.rounding_type {
                RoundingType::MorePrecision => sd_mag <= fd_mag,
                _ => sd_mag >= fd_mag,
            };
            if pick_sd {
                sd
            } else {
                fd
            }
        }
    };
    let mut r = r;
    r.neg = x.neg;
    if d.trailing_zero_display == TrailingZeroDisplay::StripIfInteger && r.is_integer() {
        r.trim_fraction_zeros_to(0);
    }
    r
}

/// Plural operands of a rounded value, honouring its visible fraction
/// digits.
pub fn operands(r: &Dec) -> PluralOperands {
    let s = r.to_plain(1, 0);
    match fixed_decimal::Decimal::try_from_str(&s) {
        Ok(d) => PluralOperands::from(&d),
        Err(_) => PluralOperands::from(r.to_f64().abs() as u64),
    }
}

/// Plural operands of a rounded significand shown with a power-of-ten
/// exponent (compact, scientific or engineering notation): the CLDR `c`
/// operand.
pub fn operands_with_exponent(r: &Dec, exponent: i32) -> PluralOperands {
    let s = r.to_plain(1, 0);
    match fixed_decimal::Decimal::try_from_str(&s) {
        Ok(d) => {
            let cd = fixed_decimal::CompactDecimal::from_significand_and_exponent(d, exponent.clamp(0, 255) as u8);
            PluralOperands::from(&cd)
        }
        Err(_) => PluralOperands::from(r.to_f64().abs() as u64),
    }
}

pub fn category_name(c: PluralCategory) -> &'static str {
    match c {
        PluralCategory::Zero => "zero",
        PluralCategory::One => "one",
        PluralCategory::Two => "two",
        PluralCategory::Few => "few",
        PluralCategory::Many => "many",
        PluralCategory::Other => "other",
    }
}

enum Piece<'a> {
    Placeholder(u8),
    Literal(&'a str),
}

/// Split a CLDR pattern with `{0}`/`{1}` placeholders.
fn split_pattern(p: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = p;
    while let Some(i) = rest.find('{') {
        if i > 0 {
            out.push(Piece::Literal(&rest[..i]));
        }
        let after = &rest[i..];
        if let Some(j) = after.find('}') {
            let n = after[1..j].parse::<u8>().unwrap_or(0);
            out.push(Piece::Placeholder(n));
            rest = &after[j + 1..];
        } else {
            out.push(Piece::Literal(after));
            rest = "";
        }
    }
    if !rest.is_empty() {
        out.push(Piece::Literal(rest));
    }
    out
}

/// Split a literal affix into whitespace (`literal`) and symbol parts.
fn split_affix(l: &str, kind: &'static str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let trimmed_start = l.trim_start();
    let lead = &l[..l.len() - trimmed_start.len()];
    let core = trimmed_start.trim_end();
    let trail = &trimmed_start[core.len()..];
    if !lead.is_empty() {
        out.push(("literal", lead.to_string()));
    }
    if !core.is_empty() {
        out.push((kind, core.to_string()));
    }
    if !trail.is_empty() {
        out.push(("literal", trail.to_string()));
    }
    out
}

fn push_literal(out: &mut Vec<Part>, l: &str) {
    if !l.is_empty() {
        out.push(part("literal", l));
    }
}

/// A currency pattern literal that may contain the minus sign.
fn push_literal_with_sign(out: &mut Vec<Part>, l: &str, minus: &str) {
    if minus.is_empty() {
        push_literal(out, l);
        return;
    }
    match l.find(minus) {
        Some(i) => {
            push_literal(out, &l[..i]);
            out.push(part("minusSign", minus));
            push_literal(out, &l[i + minus.len()..]);
        }
        None => push_literal(out, l),
    }
}

/// Interpolate a `{0}` pattern around the number parts, typing the
/// non-whitespace text of literals as `kind`.
fn interpolate_single(pattern: &str, number: Vec<Part>, kind: &'static str) -> Vec<Part> {
    let mut out = Vec::new();
    let mut slot = Some(number);
    for piece in split_pattern(pattern) {
        match piece {
            Piece::Placeholder(_) => {
                if let Some(n) = slot.take() {
                    out.extend(n);
                }
            }
            Piece::Literal(l) => {
                for (k, text) in split_affix(l, kind) {
                    out.push(part(k, text));
                }
            }
        }
    }
    if let Some(n) = slot.take() {
        out.extend(n);
    }
    out
}

fn single_to_string<'a>(items: impl Iterator<Item = PatternItem<'a, SinglePlaceholderKey>>) -> String {
    let mut s = String::new();
    for item in items {
        match item {
            PatternItem::Placeholder(_) => s.push_str("{0}"),
            PatternItem::Literal(l) => s.push_str(l),
        }
    }
    s
}

impl CurrencyData {
    fn new(locale: &Locale, code: &str, display: &CurrencyDisplay) -> Result<CurrencyData, String> {
        use icu_experimental::dimension::provider::currency::essentials::CurrencyEssentialsV1;
        use icu_experimental::dimension::provider::currency::extended::CurrencyExtendedDataV1;
        use icu_experimental::dimension::provider::currency::patterns::CurrencyPatternsDataV1;
        use icu_experimental::dimension::provider::currency::symbols::CurrencySymbolsV1;
        let code = code.to_ascii_uppercase();
        let essentials = load::<CurrencyEssentialsV1>(locale, "").ok_or("no currency essentials")?;
        let (symbol, starts, ends) = match display {
            CurrencyDisplay::Code | CurrencyDisplay::Name => (code.clone(), true, true),
            CurrencyDisplay::Symbol | CurrencyDisplay::NarrowSymbol => {
                let narrow = format!("{code}n");
                let mut found = None;
                if *display == CurrencyDisplay::NarrowSymbol {
                    found = load::<CurrencySymbolsV1>(locale, &narrow);
                }
                if found.is_none() {
                    found = load::<CurrencySymbolsV1>(locale, &code);
                }
                match found {
                    Some(p) => {
                        let s = p.get();
                        (s.as_str().to_string(), s.starts_with_letter(), s.ends_with_letter())
                    }
                    None => (code.clone(), true, true),
                }
            }
        };
        let (names, unit_patterns) = if *display == CurrencyDisplay::Name {
            (load::<CurrencyExtendedDataV1>(locale, &code), load::<CurrencyPatternsDataV1>(locale, ""))
        } else {
            (None, None)
        };
        Ok(CurrencyData { essentials, symbol, starts_with_letter: starts, ends_with_letter: ends, names, unit_patterns })
    }

    /// `{0} {1}` with the plural display name (currencyDisplay "name").
    fn name_parts(&self, nf: &NumberFormatter, number: Vec<Part>, rounded: Option<&Rounded>) -> Vec<Part> {
        let op = rounded.map(|r| operands(&r.value)).unwrap_or_else(|| PluralOperands::from(0u64));
        let name = match &self.names {
            Some(n) => n.get().get(op, &nf.rules).to_string(),
            None => self.symbol.clone(),
        };
        let pattern = match &self.unit_patterns {
            Some(p) => {
                let pat = p.get().get(op, &nf.rules);
                let mut s = String::new();
                for item in pat.iter() {
                    match item {
                        PatternItem::Placeholder(DoublePlaceholderKey::Place0) => s.push_str("{0}"),
                        PatternItem::Placeholder(DoublePlaceholderKey::Place1) => s.push_str("{1}"),
                        PatternItem::Literal(l) => s.push_str(l),
                    }
                }
                s
            }
            None => "{0} {1}".to_string(),
        };
        let mut out = Vec::new();
        let mut slot = Some(number);
        for piece in split_pattern(&pattern) {
            match piece {
                Piece::Placeholder(0) => {
                    if let Some(n) = slot.take() {
                        out.extend(n);
                    }
                }
                Piece::Placeholder(_) => out.push(part("currency", name.clone())),
                Piece::Literal(l) => push_literal(&mut out, l),
            }
        }
        out
    }
}

impl UnitData {
    fn new(tag: &str, unit: &str, display: UnitDisplay) -> Result<UnitData, String> {
        let table = unit_locale(tag);
        let w = match display {
            UnitDisplay::Long => 0,
            UnitDisplay::Short => 1,
            UnitDisplay::Narrow => 2,
        };
        let find = |u: &str| table.units.iter().find(|(n, _)| *n == u).map(|(_, p)| p[w]);
        if let Some(p) = find(unit) {
            return Ok(UnitData { patterns: p.iter().map(|(c, s)| (c.to_string(), s.to_string())).collect() });
        }
        let (a, b) = unit.split_once("-per-").ok_or_else(|| format!("unknown unit {unit}"))?;
        let pa = find(a).ok_or_else(|| format!("unknown unit {a}"))?;
        let pb = find(b).ok_or_else(|| format!("unknown unit {b}"))?;
        let denom = pb
            .iter()
            .find(|(c, _)| *c == "one")
            .or_else(|| pb.iter().find(|(c, _)| *c == "other"))
            .map(|(_, s)| s.replace("{0}", "").trim().to_string())
            .unwrap_or_default();
        let per = table.per[w];
        let patterns = pa
            .iter()
            .map(|(c, s)| (c.to_string(), per.replace("{0}", s).replace("{1}", &denom)))
            .collect();
        Ok(UnitData { patterns })
    }

    fn pattern_for(&self, category: &str) -> &str {
        self.patterns
            .iter()
            .find(|(c, _)| c == category)
            .or_else(|| self.patterns.iter().find(|(c, _)| c == "other"))
            .map(|(_, p)| p.as_str())
            .unwrap_or("{0}")
    }
}

/// Numbering systems ICU4X has decimal symbols for in some locale.
pub fn numbering_systems_with_data() -> &'static BTreeSet<String> {
    static S: OnceLock<BTreeSet<String>> = OnceLock::new();
    S.get_or_init(|| {
        let mut set = BTreeSet::new();
        if let Ok(ids) = IterableDataProvider::<icu_decimal::provider::DecimalSymbolsV1>::iter_ids(&IntlData) {
            for id in ids {
                let a = id.marker_attributes.as_str();
                if !a.is_empty() {
                    set.insert(a.to_string());
                }
            }
        }
        set
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(locale: &str) -> NumberFormatOptions {
        NumberFormatOptions {
            locale: locale.to_string(),
            numbering_system: default_numbering_system(locale),
            style: Style::Decimal,
            notation: Notation::Standard,
            grouping: Grouping::Auto,
            sign_display: SignDisplay::Auto,
            digits: Digits::decimal(),
        }
    }

    fn fmt(o: NumberFormatOptions, x: f64) -> String {
        NumberFormatter::new(o).unwrap().format(&Num::from_f64(x))
    }

    #[test]
    fn decimal_basics() {
        assert_eq!(fmt(opts("en-US"), 1234567.891), "1,234,567.891");
        assert_eq!(fmt(opts("de-DE"), 1234567.891), "1.234.567,891");
        assert_eq!(fmt(opts("en-IN"), 1234567.0), "12,34,567");
        assert_eq!(fmt(opts("en-US"), -0.0), "-0");
        assert_eq!(fmt(opts("en-US"), f64::NAN), "NaN");
        assert_eq!(fmt(opts("en-US"), f64::NEG_INFINITY), "-∞");
        assert_eq!(fmt(opts("es"), 1234.0), "1234");
        let mut o = opts("en-US");
        o.numbering_system = "arab".to_string();
        assert_eq!(fmt(o, 123.0), "١٢٣");
    }

    #[test]
    fn percent_currency_unit() {
        let mut o = opts("en-US");
        o.style = Style::Percent;
        o.digits.max_fraction = 0;
        assert_eq!(fmt(o, -1.23), "-123%");
        let mut o = opts("en-US");
        o.style = Style::Currency { code: "USD".into(), display: CurrencyDisplay::Symbol, sign: CurrencySign::Accounting };
        o.digits.min_fraction = 2;
        o.digits.max_fraction = 2;
        assert_eq!(fmt(o.clone(), -987.0), "($987.00)");
        o.sign_display = SignDisplay::Always;
        assert_eq!(fmt(o.clone(), 0.0), "+$0.00");
        o.sign_display = SignDisplay::ExceptZero;
        assert_eq!(fmt(o.clone(), -0.0), "$0.00");
        o.style = Style::Currency { code: "USD".into(), display: CurrencyDisplay::Name, sign: CurrencySign::Standard };
        o.sign_display = SignDisplay::Auto;
        assert_eq!(fmt(o.clone(), -987.0), "-987.00 US dollars");
        o.style = Style::Currency { code: "USD".into(), display: CurrencyDisplay::Code, sign: CurrencySign::Standard };
        assert_eq!(fmt(o, 987.0), "USD\u{a0}987.00");
        let mut o = opts("en-US");
        o.style = Style::Unit { unit: "kilometer-per-hour".into(), display: UnitDisplay::Long };
        assert_eq!(fmt(o.clone(), -987.0), "-987 kilometers per hour");
        o.style = Style::Unit { unit: "kilometer-per-hour".into(), display: UnitDisplay::Narrow };
        assert_eq!(fmt(o.clone(), 987.0), "987km/h");
        o.style = Style::Unit { unit: "percent".into(), display: UnitDisplay::Short };
        assert_eq!(fmt(o, -123.0), "-123%");
        assert_eq!(currency_digits("JPY"), 0);
        assert_eq!(currency_digits("USD"), 2);
    }

    #[test]
    fn notations() {
        let mut o = opts("en-US");
        o.notation = Notation::Compact { long: false };
        o.digits = Digits {
            min_fraction: 0,
            max_fraction: 0,
            min_significant: 1,
            max_significant: 2,
            rounding_type: RoundingType::MorePrecision,
            ..Digits::decimal()
        };
        assert_eq!(fmt(o.clone(), 987654321.0), "988M");
        assert_eq!(fmt(o.clone(), 9876.0), "9.9K");
        assert_eq!(fmt(o.clone(), 0.00159), "0.0016");
        assert_eq!(fmt(o.clone(), 999999.0), "1M");
        o.notation = Notation::Compact { long: true };
        assert_eq!(fmt(o.clone(), 98765.0), "99 thousand");
        let mut o = opts("en-US");
        o.notation = Notation::Scientific;
        assert_eq!(fmt(o.clone(), 987654321.0), "9.877E8");
        assert_eq!(fmt(o.clone(), 0.000123), "1.23E-4");
        o.notation = Notation::Engineering;
        assert_eq!(fmt(o.clone(), 987654321.0), "987.654E6");
    }

    #[test]
    fn ranges() {
        let mut o = opts("en-US");
        o.style = Style::Currency { code: "USD".into(), display: CurrencyDisplay::Symbol, sign: CurrencySign::Standard };
        o.digits.max_fraction = 0;
        let nf = NumberFormatter::new(o.clone()).unwrap();
        assert_eq!(nf.format_range(&Num::from_f64(3.0), &Num::from_f64(5.0)), "$3 – $5");
        assert_eq!(nf.format_range(&Num::from_f64(2.9), &Num::from_f64(3.1)), "~$3");
        o.sign_display = SignDisplay::Always;
        o.digits.min_fraction = 2;
        o.digits.max_fraction = 2;
        let nf = NumberFormatter::new(o).unwrap();
        assert_eq!(nf.format_range(&Num::from_f64(2.9), &Num::from_f64(3.1)), "+$2.90–3.10");
        let mut o = opts("pt-PT");
        o.style = Style::Currency { code: "EUR".into(), display: CurrencyDisplay::Symbol, sign: CurrencySign::Standard };
        o.digits.max_fraction = 0;
        let nf = NumberFormatter::new(o).unwrap();
        assert_eq!(nf.format_range(&Num::from_f64(3.0), &Num::from_f64(5.0)), "3 - 5\u{a0}€");
    }
}
