//! The adult and violence filters.
//!
//! Adult: Steam content descriptors 1, 3, 4 (nudity or sexual content, adult-only sexual content, general
//! mature) **or** a community tag Sexual Content / Nudity / NSFW / Hentai among the game's top tags. Descriptors
//! alone miss games ("Five Nights with Maids" has none and is tagged), so the tag check is part of the rule.
//! Violent: descriptor 2 (frequent violence or gore).

use crate::steam::Tag;

pub const ADULT_DESCRIPTORS: [u32; 3] = [1, 3, 4];
pub const VIOLENT_DESCRIPTOR: u32 = 2;
pub const ADULT_TAGS: [&str; 4] = ["Sexual Content", "Nudity", "NSFW", "Hentai"];
/// A tag counts when it is among the page's most voted tags (Steam shows about 20).
pub const TAG_WINDOW: usize = 20;

pub fn adult_by_descriptor(descriptors: &[u32]) -> bool {
    descriptors.iter().any(|d| ADULT_DESCRIPTORS.contains(d))
}

pub fn adult_by_tags(tags: &[Tag]) -> bool {
    let mut sorted: Vec<&Tag> = tags.iter().collect();
    sorted.sort_by(|a, b| b.count.cmp(&a.count));
    sorted.iter().take(TAG_WINDOW).any(|t| ADULT_TAGS.iter().any(|a| a.eq_ignore_ascii_case(&t.name)))
}

pub fn violent(descriptors: &[u32]) -> bool {
    descriptors.contains(&VIOLENT_DESCRIPTOR)
}

/// What the filters know about a game. `None`: not known yet (details or tags not cached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub adult: Option<bool>,
    pub violent: Option<bool>,
}

impl Verdict {
    pub const UNKNOWN: Verdict = Verdict { adult: None, violent: None };

    pub fn is_adult(self) -> bool {
        self.adult == Some(true)
    }

    pub fn is_violent(self) -> bool {
        self.violent == Some(true)
    }
}

/// Judge a game from what is cached. `descriptors` come from the details (`None`: not fetched), `tags` are the
/// community tags (`None`: not fetched). A game with an adult descriptor is adult without its tags.
pub fn judge(descriptors: Option<&[u32]>, tags: Option<&[Tag]>) -> Verdict {
    let adult = if descriptors.is_some_and(adult_by_descriptor) || tags.is_some_and(adult_by_tags) {
        Some(true)
    } else if descriptors.is_some() && tags.is_some() {
        Some(false)
    } else {
        None
    };
    Verdict { adult, violent: descriptors.map(violent) }
}

/// Whether a game is shown with these settings. A game the filters cannot judge yet is held back while the
/// matching filter is on, so nothing slips through while its tags are still being fetched.
pub fn shown(v: Verdict, hide_adult: bool, hide_violent: bool) -> bool {
    let adult_ok = !hide_adult || v.adult == Some(false);
    let violent_ok = !hide_violent || v.violent == Some(false);
    adult_ok && violent_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(n: &str, c: u32) -> Tag {
        Tag { id: 0, name: n.into(), count: c }
    }

    #[test]
    fn descriptors_one_three_four_are_adult_two_is_violent() {
        for d in [1, 3, 4] {
            assert!(judge(Some(&[d]), Some(&[])).is_adult());
        }
        let v = judge(Some(&[2, 5]), Some(&[]));
        assert!(v.is_violent() && !v.is_adult());
        let v = judge(Some(&[5]), Some(&[]));
        assert_eq!((v.adult, v.violent), (Some(false), Some(false)), "5 is general mature content: neither");
        assert!(judge(Some(&[1, 2]), None).is_adult(), "an adult descriptor needs no tags");
    }

    #[test]
    fn tags_catch_games_the_descriptors_miss() {
        let maids = [tag("Sexual Content", 83), tag("Hentai", 75), tag("Simulation", 64)];
        assert!(judge(Some(&[]), Some(&maids)).is_adult());
        assert!(adult_by_tags(&[tag("nsfw", 10)]), "case does not matter");
        assert!(adult_by_tags(&[tag("Nudity", 5)]));
        assert!(!adult_by_tags(&[tag("Survival", 500), tag("Co-op", 400)]));
    }

    #[test]
    fn a_stray_tag_deep_in_the_list_is_ignored() {
        let mut tags: Vec<Tag> = (0..25).map(|i| tag(&format!("T{i}"), 1000 - i)).collect();
        tags.push(tag("Nudity", 3));
        assert!(!adult_by_tags(&tags), "it is not in the top 20");
        tags.push(tag("Nudity", 999_999));
        assert!(adult_by_tags(&tags));
    }

    #[test]
    fn unknown_is_held_back_only_while_its_filter_is_on() {
        assert_eq!(judge(None, None), Verdict::UNKNOWN);
        let no_tags = judge(Some(&[]), None);
        assert_eq!(no_tags.adult, None, "tags missing: cannot say");
        assert!(!shown(no_tags, true, false));
        assert!(shown(no_tags, false, false));
        let adult = judge(Some(&[1]), None);
        assert!(!shown(adult, true, false) && shown(adult, false, false));
        let violent = judge(Some(&[2]), Some(&[]));
        assert!(!shown(violent, false, true) && shown(violent, true, false));
        assert!(shown(judge(Some(&[]), Some(&[])), true, true));
        // violent but tags unknown: the adult filter still holds it back
        assert!(!shown(judge(Some(&[2]), None), true, false));
    }
}
