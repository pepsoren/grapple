//! Controllers in games grapple starts through Proton (Epic and hand-added Windows games): how a pad is shown to
//! the game. Steam games are left to Steam Input; native games are not touched.
//!
//! Proton's Wine (`dlls/winebus.sys/main.c`, `is_hidraw_enabled`, Valve proton_10.0 and proton_11.0) hands Sony
//! pads (DualShock 4, DualSense) to the game as the real HID device ("hidraw"): games with PlayStation support see
//! a PlayStation pad, others may see nothing usable. `PROTON_DISABLE_HIDRAW=1` makes winebus skip hidraw for every
//! device, so the pad goes through SDL and shows up as an Xbox-style (XInput) controller. GE-Proton's `proton`
//! script spells the same switch `PROTON_PREFER_SDL` / `PROTON_USE_SDL` (compat option `sdlinput`, which sets
//! `PROTON_DISABLE_HIDRAW=1`), so both are set: either Proton understands one of them.
//!
//! Not offered, on purpose: an A/B swap (SDL2's `SDL_GAMECONTROLLER_USE_BUTTON_LABELS` only changes Nintendo pads
//! read through SDL, and its default already follows the printed labels) and a rumble switch (neither winebus nor
//! SDL has one that covers every pad).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a controller is shown to a Proton game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PadMode {
    /// Proton's own choice (PlayStation pads as themselves). Changes nothing.
    #[default]
    AsIs,
    /// Every pad as an Xbox-style (XInput) controller, through SDL.
    Xbox,
}

impl PadMode {
    /// The stored setting (`"as_is"`, `"xbox"`); anything else is [`PadMode::AsIs`].
    pub fn parse(s: &str) -> PadMode {
        match s {
            "xbox" => PadMode::Xbox,
            _ => PadMode::AsIs,
        }
    }

    /// The environment this mode adds to a Proton launch.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            PadMode::AsIs => &[],
            PadMode::Xbox => &[("PROTON_DISABLE_HIDRAW", "1"), ("PROTON_PREFER_SDL", "1")],
        }
    }
}

/// Add the mode's variables to a launch environment. A variable that is already there (Heroic's environment
/// options for the game) or that the session sets itself (`session_has`) is the user's own and wins.
pub fn apply(mode: PadMode, env: &mut BTreeMap<String, String>, session_has: impl Fn(&str) -> bool) {
    for (k, v) in mode.env() {
        if !env.contains_key(*k) && !session_has(k) {
            env.insert((*k).to_string(), (*v).to_string());
        }
    }
}

/// The mode for one game: its own choice, else the default.
pub fn effective(default: PadMode, per_game: Option<PadMode>) -> PadMode {
    per_game.unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_is_changes_nothing() {
        let mut env = BTreeMap::new();
        apply(PadMode::AsIs, &mut env, |_| false);
        assert!(env.is_empty());
    }

    #[test]
    fn xbox_sets_both_proton_spellings() {
        let mut env = BTreeMap::new();
        apply(PadMode::Xbox, &mut env, |_| false);
        assert_eq!(env.get("PROTON_DISABLE_HIDRAW").map(String::as_str), Some("1"));
        assert_eq!(env.get("PROTON_PREFER_SDL").map(String::as_str), Some("1"));
    }

    #[test]
    fn the_users_own_values_win() {
        // Heroic's environment option for the game
        let mut env = BTreeMap::from([("PROTON_DISABLE_HIDRAW".to_string(), "0x054C/0x0CE6".to_string())]);
        apply(PadMode::Xbox, &mut env, |_| false);
        assert_eq!(env["PROTON_DISABLE_HIDRAW"], "0x054C/0x0CE6");
        assert_eq!(env["PROTON_PREFER_SDL"], "1");
        // set in the session itself
        let mut env = BTreeMap::new();
        apply(PadMode::Xbox, &mut env, |k| k == "PROTON_PREFER_SDL");
        assert!(!env.contains_key("PROTON_PREFER_SDL"));
        assert!(env.contains_key("PROTON_DISABLE_HIDRAW"));
    }

    #[test]
    fn per_game_choice_beats_the_default() {
        assert_eq!(effective(PadMode::Xbox, None), PadMode::Xbox);
        assert_eq!(effective(PadMode::Xbox, Some(PadMode::AsIs)), PadMode::AsIs);
        assert_eq!(effective(PadMode::AsIs, Some(PadMode::Xbox)), PadMode::Xbox);
        assert_eq!(PadMode::parse("xbox"), PadMode::Xbox);
        assert_eq!(PadMode::parse("as_is"), PadMode::AsIs);
        assert_eq!(PadMode::parse("weird"), PadMode::AsIs);
    }
}
