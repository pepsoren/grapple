//! Money: minor units per currency, conversion through the daily rates, the Steam country -> currency table.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An amount in minor units (cents) of a currency. In `StorePrice` the display ones are converted, the
/// `native` ones are what the store charges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    pub minor: i64,
    pub currency: String,
}

impl Money {
    pub fn new(minor: i64, currency: impl Into<String>) -> Money {
        Money { minor, currency: currency.into() }
    }
}

/// Digits after the decimal point (ISO 4217). Everything not listed has 2.
pub fn decimals(currency: &str) -> u32 {
    match currency {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "UGX" | "PYG" | "XAF" | "XOF" | "XPF" | "RWF" | "KMF" | "GNF" | "DJF" | "VUV" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" | "IQD" | "LYD" => 3,
        _ => 2,
    }
}

/// Major units (what a person reads: 123.45) of an amount in minor units.
pub fn to_major(minor: i64, currency: &str) -> f64 {
    minor as f64 / 10f64.powi(decimals(currency) as i32)
}

pub fn from_major(major: f64, currency: &str) -> i64 {
    (major * 10f64.powi(decimals(currency) as i32)).round() as i64
}

/// Steam reports every price in hundredths of the major unit, even for currencies without decimals
/// (JPY 1980 comes as 198000). Turns Steam's number into [`Money`] in ISO minor units.
pub fn from_steam(amount: i64, currency: &str) -> Money {
    Money::new(from_major(amount as f64 / 100.0, currency), currency)
}

/// Rates: units of each currency per 1 USD (`open.er-api.com` shape).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rates {
    pub per_usd: HashMap<String, f64>,
}

impl Rates {
    pub fn new(per_usd: HashMap<String, f64>) -> Rates {
        let mut per_usd = per_usd;
        per_usd.insert("USD".into(), 1.0);
        Rates { per_usd }
    }

    pub fn is_empty(&self) -> bool {
        self.per_usd.len() <= 1
    }

    pub fn has(&self, currency: &str) -> bool {
        self.per_usd.contains_key(currency)
    }

    /// `amount` (minor units of `from`) in minor units of `to`; `None` when a rate is missing.
    pub fn convert(&self, amount: i64, from: &str, to: &str) -> Option<i64> {
        if from == to {
            return Some(amount);
        }
        let f = *self.per_usd.get(from)?;
        let t = *self.per_usd.get(to)?;
        if f <= 0.0 || t <= 0.0 {
            return None;
        }
        Some(from_major(to_major(amount, from) / f * t, to))
    }

    /// Like [`Rates::convert`], keeping the amount as it is when no rate is known (the UI then shows the native
    /// price in its own currency; only possible before the first rates fetch).
    pub fn convert_or_keep(&self, m: &Money, to: &str) -> Money {
        match self.convert(m.minor, &m.currency, to) {
            Some(v) => Money::new(v, to),
            None => m.clone(),
        }
    }
}

/// The currency Steam charges in for a store country (`cc`). Steam has its own regional currencies for many
/// countries and USD for the rest. Only used for the default display currency before Steam has priced anything;
/// every real price carries the currency Steam answered with.
pub fn steam_currency_for(country: &str) -> &'static str {
    match country.to_ascii_uppercase().as_str() {
        "ZA" => "ZAR",
        "US" | "PR" | "EC" | "SV" | "PA" => "USD",
        "GB" => "GBP",
        "AT" | "BE" | "CY" | "EE" | "FI" | "FR" | "DE" | "GR" | "IE" | "IT" | "LV" | "LT" | "LU" | "MT" | "NL" | "PT" | "SK" | "SI" | "ES" | "HR" | "AD" | "MC" | "SM" | "VA" | "ME" | "XK" => "EUR",
        "CA" => "CAD",
        "AU" => "AUD",
        "NZ" => "NZD",
        "JP" => "JPY",
        "KR" => "KRW",
        "CN" => "CNY",
        "HK" => "HKD",
        "TW" => "TWD",
        "SG" => "SGD",
        "MY" => "MYR",
        "TH" => "THB",
        "ID" => "IDR",
        "PH" => "PHP",
        "VN" => "VND",
        "IN" => "INR",
        "PK" => "USD",
        "BR" => "BRL",
        "MX" => "MXN",
        "AR" => "ARS",
        "CL" => "CLP",
        "CO" => "COP",
        "PE" => "PEN",
        "UY" => "UYU",
        "CR" => "CRC",
        "RU" => "RUB",
        "UA" => "UAH",
        "KZ" => "KZT",
        "TR" => "TRY",
        "PL" => "PLN",
        "CH" | "LI" => "CHF",
        "NO" => "NOK",
        "SE" => "SEK",
        "DK" => "DKK",
        "CZ" => "CZK",
        "HU" => "HUF",
        "RO" => "RON",
        "BG" => "BGN",
        "IL" => "ILS",
        "SA" => "SAR",
        "AE" => "AED",
        "QA" => "QAR",
        "KW" => "KWD",
        _ => "USD",
    }
}

/// Names for the currency picker. Codes without a name here still work; the picker shows the code.
const NAMES: &[(&str, &str)] = &[
    ("USD", "US dollar"),
    ("EUR", "Euro"),
    ("GBP", "British pound"),
    ("ZAR", "South African rand"),
    ("MUR", "Mauritian rupee"),
    ("CAD", "Canadian dollar"),
    ("AUD", "Australian dollar"),
    ("NZD", "New Zealand dollar"),
    ("JPY", "Japanese yen"),
    ("KRW", "South Korean won"),
    ("CNY", "Chinese yuan"),
    ("HKD", "Hong Kong dollar"),
    ("TWD", "New Taiwan dollar"),
    ("SGD", "Singapore dollar"),
    ("MYR", "Malaysian ringgit"),
    ("THB", "Thai baht"),
    ("IDR", "Indonesian rupiah"),
    ("PHP", "Philippine peso"),
    ("VND", "Vietnamese dong"),
    ("INR", "Indian rupee"),
    ("BRL", "Brazilian real"),
    ("MXN", "Mexican peso"),
    ("ARS", "Argentine peso"),
    ("CLP", "Chilean peso"),
    ("COP", "Colombian peso"),
    ("PEN", "Peruvian sol"),
    ("UYU", "Uruguayan peso"),
    ("RUB", "Russian ruble"),
    ("UAH", "Ukrainian hryvnia"),
    ("KZT", "Kazakhstani tenge"),
    ("TRY", "Turkish lira"),
    ("PLN", "Polish zloty"),
    ("CHF", "Swiss franc"),
    ("NOK", "Norwegian krone"),
    ("SEK", "Swedish krona"),
    ("DKK", "Danish krone"),
    ("CZK", "Czech koruna"),
    ("HUF", "Hungarian forint"),
    ("RON", "Romanian leu"),
    ("BGN", "Bulgarian lev"),
    ("ILS", "Israeli shekel"),
    ("SAR", "Saudi riyal"),
    ("AED", "UAE dirham"),
    ("QAR", "Qatari riyal"),
    ("KWD", "Kuwaiti dinar"),
    ("EGP", "Egyptian pound"),
    ("NGN", "Nigerian naira"),
    ("KES", "Kenyan shilling"),
    ("PKR", "Pakistani rupee"),
    ("BDT", "Bangladeshi taka"),
];

pub fn currency_name(code: &str) -> Option<&'static str> {
    NAMES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

/// A tier price that reads well ("R 100", "$ 5"): the 1-2-5 number closest to `major` on a log scale.
pub fn nice_round(major: f64) -> f64 {
    if major <= 0.0 || !major.is_finite() {
        return 5.0;
    }
    let exp = major.log10().floor();
    let base = 10f64.powf(exp);
    let cands = [1.0, 2.0, 5.0, 10.0].map(|m| m * base);
    cands
        .into_iter()
        .min_by(|a, b| (a / major).ln().abs().total_cmp(&(b / major).ln().abs()))
        .unwrap_or(major)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates() -> Rates {
        Rates::new([("ZAR".to_string(), 16.5), ("JPY".to_string(), 150.0), ("EUR".to_string(), 0.9)].into())
    }

    #[test]
    fn converts_through_usd_and_keeps_currency_digits() {
        let r = rates();
        assert_eq!(r.convert(1650, "ZAR", "USD"), Some(100));
        assert_eq!(r.convert(100, "USD", "ZAR"), Some(1650));
        assert_eq!(r.convert(100, "USD", "JPY"), Some(150), "yen has no minor unit");
        assert_eq!(r.convert(1500, "JPY", "USD"), Some(1000));
        assert_eq!(r.convert(500, "ZAR", "ZAR"), Some(500), "same currency is exact");
        assert_eq!(r.convert(500, "ZAR", "XXX"), None);
    }

    #[test]
    fn steam_amounts_are_hundredths_even_for_yen() {
        assert_eq!(from_steam(11100, "ZAR"), Money::new(11100, "ZAR"));
        assert_eq!(from_steam(198000, "JPY"), Money::new(1980, "JPY"));
    }

    #[test]
    fn countries_map_to_steam_currencies() {
        assert_eq!(steam_currency_for("za"), "ZAR");
        assert_eq!(steam_currency_for("DE"), "EUR");
        assert_eq!(steam_currency_for("MU"), "USD");
    }

    #[test]
    fn nice_round_picks_one_two_five() {
        assert_eq!(nice_round(99.0), 100.0);
        assert_eq!(nice_round(6.0), 5.0);
        assert_eq!(nice_round(920.0), 1000.0);
        assert_eq!(nice_round(3.0), 2.0);
    }
}
