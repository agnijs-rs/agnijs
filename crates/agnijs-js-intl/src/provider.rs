//! The crate's own baked data provider.
//!
//! The ICU4X formatters use their crates' compiled data through their
//! constructors. A few ECMA-402 operations need the raw CLDR records
//! instead (which locales have data, the decimal symbols of a numbering
//! system, currency patterns and names, unit names, percent patterns), so
//! this provider bakes the same data crates into a struct this crate can
//! query directly, including enumeration of the locales each marker holds.

// Each macro is imported by name from the crate that owns its marker (and,
// for the `ITER` form, the inner macro it expands to). With the
// crate-default data every data crate holds only its own markers; with a
// custom `ICU4X_DATA_DIR` (docs/binary-size.md) every data crate includes
// the same generated module, so glob imports would be ambiguous.
use icu_datetime_data::{
    __impl_datetime_names_month_gregorian_v1, __impl_datetime_patterns_date_gregorian_v1,
    impl_datetime_names_month_gregorian_v1, impl_datetime_patterns_date_gregorian_v1,
};
use icu_decimal_data::{
    __impl_decimal_symbols_v1, impl_decimal_compact_long_v1, impl_decimal_compact_short_v1,
    impl_decimal_symbols_v1, make_provider,
};
use icu_experimental_data::{
    __impl_currency_displayname_v1, impl_currency_displayname_v1, impl_currency_essentials_v1,
    impl_currency_extended_data_v1, impl_currency_fractions_v1, impl_currency_patterns_data_v1,
    impl_currency_symbols_v1, impl_decimal_percent_v1,
};
use icu_list_data::{__impl_list_and_v1, impl_list_and_v1};
use icu_plurals_data::{__impl_plurals_cardinal_v1, impl_plurals_cardinal_v1};

#[allow(unused_imports)]
pub(crate) mod icu {
    pub use icu_datetime as datetime;
    pub use icu_decimal as decimal;
    pub use icu_experimental as experimental;
    pub use icu_list as list;
    pub use icu_plurals as plurals;
}

extern crate alloc;

pub(crate) struct IntlData;

make_provider!(IntlData);
impl_decimal_symbols_v1!(IntlData, ITER);
impl_decimal_compact_short_v1!(IntlData);
impl_decimal_compact_long_v1!(IntlData);
impl_datetime_names_month_gregorian_v1!(IntlData, ITER);
impl_datetime_patterns_date_gregorian_v1!(IntlData, ITER);
impl_list_and_v1!(IntlData, ITER);
impl_plurals_cardinal_v1!(IntlData, ITER);
impl_decimal_percent_v1!(IntlData);
impl_currency_essentials_v1!(IntlData);
impl_currency_symbols_v1!(IntlData);
impl_currency_extended_data_v1!(IntlData);
impl_currency_patterns_data_v1!(IntlData);
impl_currency_fractions_v1!(IntlData);
impl_currency_displayname_v1!(IntlData, ITER);
