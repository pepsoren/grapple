//! The shapes the UI gets, over IPC in camelCase. They mirror `docs/STORE_PLAN.md` ("Shared contract") and
//! `app/ui/src/lib/store-types.ts` exactly; the few extra types (filters, detail, currency, alerts) are
//! additions that do not change any contract shape.

use crate::money::Money;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum StoreKind {
    Steam,
    Epic,
}

impl StoreKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StoreKind::Steam => "steam",
            StoreKind::Epic => "epic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorePrice {
    pub store: StoreKind,
    /// Regular price and price now, in the display currency.
    pub original: Money,
    #[serde(rename = "final")]
    pub final_price: Money,
    pub discount_pct: u32,
    /// What the store charges, in its own currency (for checkout).
    pub native: Money,
    /// Where Buy goes.
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreArt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    pub shots: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SimilarKind {
    Game,
    Friend,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Similar {
    pub kind: SimilarKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreGame {
    pub key: String,
    pub name: String,
    pub art: StoreArt,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub short: String,
    pub developers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviews: Option<u64>,
    pub free: bool,
    pub coming_soon: bool,
    pub prices: Vec<StorePrice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cheapest: Option<StoreKind>,
    pub owned: bool,
    pub wishlisted: bool,
    pub notify_on_sale: bool,
    pub similar: Vec<Similar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lowest_seen: Option<Money>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Under {
    pub tier_minor: i64,
    pub games: Vec<StoreGame>,
    pub hidden: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewReleases {
    pub games: Vec<StoreGame>,
    pub hidden: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpicFreeItem {
    pub game: StoreGame,
    pub now: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenreTile {
    pub name: String,
    pub art: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreFront {
    pub featured: Vec<StoreGame>,
    pub picked: Vec<StoreGame>,
    pub top: Vec<StoreGame>,
    pub deals: Vec<StoreGame>,
    pub under: Under,
    pub new_releases: NewReleases,
    pub wishlist: Vec<StoreGame>,
    pub epic_free: Vec<EpicFreeItem>,
    pub genres: Vec<GenreTile>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoresOn {
    pub steam: bool,
    pub epic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreSettings {
    pub display_currency: String,
    pub hide_adult: bool,
    pub hide_violent: bool,
    pub stores: StoresOn,
    pub wishlist_alerts: bool,
    /// Whether an IsThereAnyDeal key is set. The key itself never leaves the keyring.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itad_key: Option<bool>,
}

// ------------------------------------------------------------------------------------------ additions

/// `store_game(key)`: the game plus "More like this".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreGameDetail {
    #[serde(flatten)]
    pub game: StoreGame,
    pub more_like_this: Vec<StoreGame>,
}

/// A Steam-wishlist game Josh removed from grapple's list (grapple never touches the Steam wishlist itself).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HiddenWish {
    pub key: String,
    pub name: String,
}

/// Filters of `store_search`. Everything is optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StoreFilters {
    pub genre: Option<String>,
    pub tag: Option<String>,
    /// Display-currency minor units.
    pub max_price_minor: Option<i64>,
    pub on_sale: Option<bool>,
    /// Restrict to one store's games.
    pub store: Option<StoreKind>,
    /// Show games the adult/violent filters would hide (the "Show" link).
    pub include_hidden: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyInfo {
    pub code: String,
    pub name: String,
    /// Digits after the point (JPY 0, ZAR 2): the UI formats `minor` with it.
    pub decimals: u32,
}

/// What `store_region_get` returns: where the Steam prices come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Region {
    pub country: String,
    pub currency: String,
    /// `setting`, `env`, `steam-config`, `steam-log` or `default`.
    pub source: String,
}

/// Emitted when a wishlisted game goes on sale (`store-sale`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaleAlert {
    pub key: String,
    pub name: String,
    pub store: StoreKind,
    pub discount_pct: u32,
    #[serde(rename = "final")]
    pub final_price: Money,
}

/// What `store-changed` says: which parts of the store page changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreChanged {
    /// `front`, `wishlist`, `settings`, `prices` ... (informational; the UI may just refetch).
    pub sections: Vec<String>,
}

/// A game in the library, as the store needs it for ownership and taste.
#[derive(Debug, Clone, PartialEq)]
pub struct OwnedGame {
    /// `steam:730` or `epic:<app_name>`
    pub id: String,
    pub name: String,
    pub playtime_min: u64,
    pub last_played: Option<i64>,
    /// When it was added to the library, if known locally.
    pub added_at: Option<i64>,
    /// grapple's own icon/cover URL for the "like" stack.
    pub icon: Option<String>,
    /// Epic: the catalog namespace (ownership of `epic:<namespace>:<offer>` entries).
    pub epic_namespace: Option<String>,
}

impl OwnedGame {
    pub fn steam_appid(&self) -> Option<u32> {
        self.id.strip_prefix("steam:")?.parse().ok()
    }
}

/// A tether friend and the game they are playing.
#[derive(Debug, Clone, PartialEq)]
pub struct Friend {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    pub playing_appid: Option<u32>,
    pub playing_name: Option<String>,
}

/// What the app tells the store about the outside world.
pub trait Library: Send + Sync + 'static {
    fn owned(&self) -> Vec<OwnedGame>;
    fn friends(&self) -> Vec<Friend>;
    /// Josh's Steam account (steamid64), for the public wishlist.
    fn steam_id64(&self) -> Option<u64>;
    /// Steam's own country code for the store, when its files name one.
    fn steam_region(&self) -> Option<(String, &'static str)>;
}

/// Events the service raises; the app forwards them as Tauri events.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreEvent {
    Changed(StoreChanged),
    Sale(SaleAlert),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wire_shapes_match_the_contract() {
        let p = StorePrice {
            store: StoreKind::Steam,
            original: Money::new(18500, "ZAR"),
            final_price: Money::new(11100, "ZAR"),
            discount_pct: 40,
            native: Money::new(11100, "ZAR"),
            url: "https://store.steampowered.com/app/1".into(),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v,
            json!({"store":"steam","original":{"minor":18500,"currency":"ZAR"},"final":{"minor":11100,"currency":"ZAR"},
                   "discountPct":40,"native":{"minor":11100,"currency":"ZAR"},"url":"https://store.steampowered.com/app/1"})
        );
        let s = StoreSettings { display_currency: "ZAR".into(), hide_adult: true, hide_violent: false, stores: StoresOn { steam: true, epic: true }, wishlist_alerts: true, itad_key: Some(false) };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"displayCurrency":"ZAR","hideAdult":true,"hideViolent":false,"stores":{"steam":true,"epic":true},"wishlistAlerts":true,"itadKey":false})
        );
        let g = StoreGame {
            key: "steam:1".into(),
            name: "X".into(),
            art: StoreArt { cover: Some("grapple://store/steam%3A1/cover".into()), shots: vec![], ..Default::default() },
            genres: vec![],
            tags: vec![],
            short: String::new(),
            developers: vec![],
            release: None,
            reviews: None,
            free: false,
            coming_soon: true,
            prices: vec![],
            cheapest: None,
            owned: false,
            wishlisted: true,
            notify_on_sale: true,
            similar: vec![Similar { kind: SimilarKind::Friend, id: "9".into(), icon: None, name: "kai".into() }],
            lowest_seen: None,
        };
        let v = serde_json::to_value(&g).unwrap();
        assert_eq!(v["comingSoon"], true);
        assert_eq!(v["notifyOnSale"], true);
        assert!(v.get("release").is_none() && v.get("lowestSeen").is_none() && v["art"].get("hero").is_none());
        assert_eq!(v["similar"][0], json!({"kind":"friend","id":"9","name":"kai"}));
        let d = StoreGameDetail { game: g, more_like_this: vec![] };
        let v = serde_json::to_value(&d).unwrap();
        assert!(v["moreLikeThis"].is_array() && v["key"] == "steam:1", "flattened");
    }

    #[test]
    fn settings_come_back_from_camel_case() {
        let s: StoreSettings = serde_json::from_value(json!({"displayCurrency":"EUR","hideAdult":false,"hideViolent":true,"stores":{"steam":true,"epic":false},"wishlistAlerts":false})).unwrap();
        assert_eq!(s.display_currency, "EUR");
        assert!(!s.stores.epic && s.itad_key.is_none());
    }
}
