//! Recommendations: explainable scoring.
//!
//! * **Taste profile**: for every owned game that has tags, a weight from its playtime (log-scaled, recent play
//!   counted up) plus a flat bonus when it was added to the library lately (the purchase pattern: the best
//!   signal available locally). The game's weighted tags and genres are added into one profile vector.
//! * A candidate's **taste** is the cosine between the profile and its own weighted tags.
//! * **Price fit**: cheaper and discounted games score a little higher; `price_weight` raises that for the
//!   Home picks.
//! * **Friends** who play it, **wishlist** and a touch of popularity.
//! * `similar`: the 2-3 owned games sharing the most weighted tags with the candidate (the icon stack).
//!
//! Exclusions (owned, filtered, unreleased) happen before scoring.

use crate::steam::Tag;
use crate::types::OwnedGame;
use std::collections::HashMap;

/// (lowercase tag or genre, weight 0..1)
pub type TagVec = Vec<(String, f64)>;

const TOP_TAGS: usize = 12;
const GENRE_WEIGHT: f64 = 0.6;

/// The weighted tags and genres of one game.
pub fn tag_vector(genres: &[String], tags: &[Tag]) -> TagVec {
    let mut m: HashMap<String, f64> = HashMap::new();
    let mut sorted: Vec<&Tag> = tags.iter().collect();
    sorted.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    let max = sorted.first().map_or(1.0, |t| t.count.max(1) as f64);
    for t in sorted.iter().take(TOP_TAGS) {
        let w = (t.count as f64 / max).powf(0.7);
        let e = m.entry(t.name.to_lowercase()).or_insert(0.0);
        *e = e.max(w);
    }
    for g in genres {
        let e = m.entry(g.to_lowercase()).or_insert(0.0);
        *e = e.max(GENRE_WEIGHT);
    }
    let mut v: TagVec = m.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Cosine similarity of two weighted tag sets, 0..1.
pub fn cosine(a: &TagVec, b: &TagVec) -> f64 {
    let na = a.iter().map(|(_, w)| w * w).sum::<f64>().sqrt();
    let nb = b.iter().map(|(_, w)| w * w).sum::<f64>().sqrt();
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    let dot: f64 = a.iter().filter_map(|(t, w)| Some(w * b.iter().find(|(u, _)| u == t)?.1)).sum();
    dot / (na * nb)
}

#[derive(Debug, Clone)]
pub struct ProfileGame {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub weight: f64,
    pub vec: TagVec,
}

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub weights: HashMap<String, f64>,
    norm: f64,
    max_weight: f64,
    pub games: Vec<ProfileGame>,
}

/// How much one owned game says about taste.
pub fn game_weight(g: &OwnedGame, now: i64) -> f64 {
    let hours = g.playtime_min as f64 / 60.0;
    let mut w = (1.0 + hours).ln();
    if let Some(last) = g.last_played {
        let days = (now - last).max(0) as f64 / 86_400.0;
        w *= if days <= 14.0 {
            2.0
        } else if days <= 60.0 {
            1.5
        } else if days <= 180.0 {
            1.2
        } else {
            1.0
        };
    }
    if let Some(added) = g.added_at
        && (now - added) as f64 / 86_400.0 <= 120.0
    {
        w += 0.8;
    }
    // Owned counts for something even when never played.
    w.max(0.15)
}

pub fn build_profile(items: Vec<(OwnedGame, TagVec)>, now: i64) -> Profile {
    let mut p = Profile::default();
    for (g, vec) in items {
        if vec.is_empty() {
            continue;
        }
        let weight = game_weight(&g, now);
        for (t, w) in &vec {
            *p.weights.entry(t.clone()).or_insert(0.0) += weight * w;
        }
        p.games.push(ProfileGame { id: g.id, name: g.name, icon: g.icon, weight, vec });
    }
    p.norm = p.weights.values().map(|w| w * w).sum::<f64>().sqrt();
    p.max_weight = p.weights.values().copied().fold(0.0, f64::max);
    p
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// Cosine similarity between the profile and a game's tags, 0..1.
    pub fn taste(&self, cand: &TagVec) -> f64 {
        if self.norm <= 0.0 || cand.is_empty() {
            return 0.0;
        }
        let cn = cand.iter().map(|(_, w)| w * w).sum::<f64>().sqrt();
        if cn <= 0.0 {
            return 0.0;
        }
        let dot: f64 = cand.iter().map(|(t, w)| self.weights.get(t).copied().unwrap_or(0.0) * w).sum();
        dot / (self.norm * cn)
    }

    /// The owned games that share the most weighted tags with the candidate, best first. Needs at least two
    /// strong shared tags' worth of overlap.
    pub fn similar(&self, cand: &TagVec, max: usize) -> Vec<&ProfileGame> {
        let mut scored: Vec<(f64, &ProfileGame)> = self
            .games
            .iter()
            .map(|g| {
                let shared: f64 = g
                    .vec
                    .iter()
                    .filter_map(|(t, gw)| {
                        let cw = cand.iter().find(|(c, _)| c == t)?.1;
                        let rarity = 0.5 + 0.5 * (self.weights.get(t).copied().unwrap_or(0.0) / self.max_weight.max(f64::EPSILON));
                        Some(gw.min(cw) * rarity)
                    })
                    .sum();
                (shared, g)
            })
            .filter(|(s, _)| *s >= 1.0)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.weight.total_cmp(&a.1.weight)).then_with(|| a.1.id.cmp(&b.1.id)));
        scored.into_iter().take(max).map(|(_, g)| g).collect()
    }
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub vec: TagVec,
    /// 0..100
    pub discount: u32,
    /// Price now in USD, 0 for free.
    pub price_usd: f64,
    pub reviews: Option<u64>,
    pub wishlisted: bool,
    pub friends: usize,
}

/// 1 for free, falling towards 0 as the price rises (half at about 30 USD).
pub fn price_fit(price_usd: f64) -> f64 {
    1.0 / (1.0 + (price_usd.max(0.0) / 30.0).powf(1.5))
}

pub fn score(profile: &Profile, c: &Candidate, price_weight: f64) -> f64 {
    let taste = profile.taste(&c.vec);
    let pop = c.reviews.map_or(0.0, |r| ((1.0 + r as f64).ln() / (1.0 + 100_000f64).ln()).min(1.0));
    taste
        + 0.15 * price_weight * price_fit(c.price_usd)
        + 0.35 * price_weight * (c.discount as f64 / 100.0)
        + if c.wishlisted { 0.2 } else { 0.0 }
        + (0.12 * c.friends as f64).min(0.3)
        + 0.05 * pop
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(n: &str, c: u32) -> Tag {
        Tag { id: 0, name: n.into(), count: c }
    }

    fn owned(id: &str, min: u64, last_days_ago: Option<i64>, added_days_ago: Option<i64>, now: i64) -> OwnedGame {
        OwnedGame {
            id: id.into(),
            name: id.into(),
            playtime_min: min,
            last_played: last_days_ago.map(|d| now - d * 86_400),
            added_at: added_days_ago.map(|d| now - d * 86_400),
            icon: Some(format!("icon:{id}")),
            epic_namespace: None,
        }
    }

    const NOW: i64 = 1_800_000_000;

    fn survival() -> TagVec {
        tag_vector(&["Survival".into()], &[tag("Survival", 100), tag("Crafting", 90), tag("Open World", 80), tag("Co-op", 70)])
    }

    fn shooter() -> TagVec {
        tag_vector(&["Action".into()], &[tag("FPS", 100), tag("Shooter", 95), tag("Multiplayer", 60)])
    }

    #[test]
    fn tag_vectors_are_normalised_and_lowercase() {
        let v = survival();
        assert_eq!(v[0].0, "survival");
        assert!((v[0].1 - 1.0).abs() < 1e-9);
        assert!(v.iter().all(|(_, w)| *w > 0.0 && *w <= 1.0));
        let genre_only = tag_vector(&["RPG".into()], &[]);
        assert_eq!(genre_only, vec![("rpg".to_string(), GENRE_WEIGHT)]);
    }

    #[test]
    fn playtime_recency_and_new_purchases_raise_weight() {
        let old = game_weight(&owned("a", 6000, Some(400), None, NOW), NOW);
        let recent = game_weight(&owned("a", 6000, Some(3), None, NOW), NOW);
        assert!(recent > 1.9 * old);
        let bought = game_weight(&owned("b", 0, None, Some(10), NOW), NOW);
        let unplayed = game_weight(&owned("b", 0, None, Some(900), NOW), NOW);
        assert!(bought > unplayed + 0.6);
        assert!(unplayed >= 0.15, "owned counts for a little even unplayed");
        assert!(game_weight(&owned("c", 60 * 100, None, None, NOW), NOW) > game_weight(&owned("c", 60 * 10, None, None, NOW), NOW));
    }

    fn profile() -> Profile {
        build_profile(
            vec![
                (owned("steam:252490", 399 * 60, Some(5), None, NOW), survival()),
                (owned("steam:892970", 89 * 60, Some(40), None, NOW), survival()),
                (owned("steam:730", 262 * 60, Some(2), None, NOW), shooter()),
                (owned("steam:99", 100, None, None, NOW), vec![]), // no tags: ignored
            ],
            NOW,
        )
    }

    #[test]
    fn taste_prefers_what_the_library_plays() {
        let p = profile();
        assert_eq!(p.games.len(), 3);
        let like_survival = p.taste(&survival());
        let like_shooter = p.taste(&shooter());
        let unrelated = p.taste(&tag_vector(&["Casual".into()], &[tag("Puzzle", 100), tag("Relaxing", 80)]));
        assert!(like_survival > unrelated && like_shooter > unrelated);
        assert!(unrelated < 0.05);
        assert!(like_survival > 0.0 && like_survival <= 1.0);
        assert_eq!(Profile::default().taste(&survival()), 0.0, "no library, no taste");
    }

    #[test]
    fn similar_names_the_owned_games_that_share_tags() {
        let p = profile();
        let s = p.similar(&survival(), 3);
        let ids: Vec<&str> = s.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["steam:252490", "steam:892970"], "the two survival games, the shooter shares nothing");
        assert!(p.similar(&tag_vector(&[], &[tag("Puzzle", 10)]), 3).is_empty());
        assert_eq!(p.similar(&survival(), 1).len(), 1);
    }

    #[test]
    fn price_discount_wishlist_and_friends_move_the_score() {
        let p = profile();
        let base = Candidate { vec: survival(), discount: 0, price_usd: 25.0, reviews: Some(10_000), wishlisted: false, friends: 0 };
        let s0 = score(&p, &base, 1.0);
        assert!(score(&p, &Candidate { discount: 50, ..base.clone() }, 1.0) > s0 + 0.15);
        assert!(score(&p, &Candidate { wishlisted: true, ..base.clone() }, 1.0) > s0 + 0.19);
        assert!(score(&p, &Candidate { friends: 2, ..base.clone() }, 1.0) > s0 + 0.2);
        assert!(score(&p, &Candidate { friends: 20, ..base.clone() }, 1.0) < s0 + 0.31, "friends are capped");
        assert!(score(&p, &Candidate { price_usd: 0.0, ..base.clone() }, 1.0) > s0);
        // the Home picks weigh price a little higher
        let d = Candidate { discount: 60, ..base.clone() };
        assert!(score(&p, &d, 1.5) - score(&p, &base, 1.5) > score(&p, &d, 1.0) - score(&p, &base, 1.0));
    }
}
