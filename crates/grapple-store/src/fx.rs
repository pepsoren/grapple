//! Daily exchange rates from `open.er-api.com` (free, no key). One fetch a day, cached in `kv`.

use crate::error::{Error, Result};
use crate::money::Rates;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FxSnapshot {
    pub per_usd: HashMap<String, f64>,
    /// When the provider says the rates were computed, unix seconds.
    pub as_of: i64,
}

impl FxSnapshot {
    pub fn rates(&self) -> Rates {
        Rates::new(self.per_usd.clone())
    }
}

pub fn parse(text: &str) -> Result<FxSnapshot> {
    let v: Value = serde_json::from_str(text)?;
    if v.get("result").and_then(Value::as_str) != Some("success") {
        return Err(Error::Parse("fx: provider did not answer with success".into()));
    }
    let rates = v.get("rates").and_then(Value::as_object).ok_or_else(|| Error::Parse("fx: no rates".into()))?;
    let per_usd: HashMap<String, f64> = rates.iter().filter_map(|(k, x)| Some((k.clone(), x.as_f64().filter(|r| *r > 0.0)?))).collect();
    if !per_usd.contains_key("USD") || per_usd.len() < 2 {
        return Err(Error::Parse("fx: rates look empty".into()));
    }
    Ok(FxSnapshot { per_usd, as_of: v.get("time_last_update_unix").and_then(Value::as_i64).unwrap_or(0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_recorded_answer() {
        let text = std::fs::read_to_string(format!("{}/tests/fixtures/fx.json", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let s = parse(&text).unwrap();
        assert_eq!(s.per_usd["USD"], 1.0);
        assert!(s.per_usd["ZAR"] > 5.0);
        assert!(s.as_of > 1_700_000_000);
        assert!(s.rates().has("EUR"));
    }

    #[test]
    fn refuses_error_answers() {
        assert!(parse("{\"result\":\"error\",\"error-type\":\"quota-reached\"}").is_err());
        assert!(parse("{\"result\":\"success\",\"rates\":{}}").is_err());
        assert!(parse("<html>").is_err());
    }
}
