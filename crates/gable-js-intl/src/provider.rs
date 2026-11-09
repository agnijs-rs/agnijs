//! The crate's own baked data provider.
//!
//! The ICU4X formatters use their crates' compiled data through their
//! constructors. A few ECMA-402 operations need the raw CLDR records
//! instead (which locales have data, the decimal symbols of a numbering
//! system, currency patterns and names, unit names, percent patterns), so
//! this provider bakes the same data crates into a struct this crate can
//! query directly, including enumeration of the locales each marker holds.

#[allow(unused_imports)]
use icu_datetime_data::*;
#[allow(unused_imports)]
use icu_decimal_data::*;
#[allow(unused_imports)]
use icu_experimental_data::*;
#[allow(unused_imports)]
use icu_list_data::*;
#[allow(unused_imports)]
use icu_plurals_data::*;

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

icu_decimal_data::make_provider!(IntlData);
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
