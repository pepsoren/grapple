//! Game invites, the parts that do not need tether (both backends use them): the `grapple://invite/<id>` deep
//! link, what Play on an invite means on this computer, and the shapes the UI gets (`invites-changed`,
//! `invite-play`; app/ui/src/lib/types.ts mirrors them). The tether side is `tether_svc.rs` (real backend).
//!
//! What Play does here, decided from the library, never from the invite: an installed game is launched through
//! the normal launch path, a game in the library that is not installed opens the install sheet, and a game this
//! computer does not have opens grapple's store page (Steam, Epic) or just says so (games added by hand).

use grapple_core::{Game, Store, sort_name};
use serde::Serialize;

/// The deep link's id: `grapple://invite/<1-20 digits>` exactly (tether's `GAME_INVITE_LINK_RE`): no query,
/// fragment, trailing slash, other path or other case. Anything else is `None` and nothing happens.
pub fn parse_invite_link(link: &str) -> Option<String> {
    let id = link.strip_prefix("grapple://invite/")?;
    ((1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_string())
}

/// The invite in a command line (`grapple grapple://invite/<id>`, as the desktop's URL handler starts it, or a
/// second launch handing its arguments over). Arguments that are not exactly such a link are ignored.
pub fn invite_link_arg(args: &[String]) -> Option<String> {
    args.iter().find_map(|a| parse_invite_link(a))
}

/// A library game as an invite's Play sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibGame {
    pub id: String,
    /// `sort_name` of the name (the fallback match for games added by hand, whose ids differ per computer).
    pub key: String,
    pub installed: bool,
    pub store: Store,
}

impl LibGame {
    pub fn of(g: &Game) -> LibGame {
        LibGame { id: g.id.0.clone(), key: sort_name(&g.name), installed: g.installed, store: g.store }
    }
}

/// What Play on an invite does on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlayAction {
    /// Installed: start it (the normal launch path).
    Launch { game: String },
    /// In the library, not installed: the install sheet.
    Install { game: String },
    /// Not here: grapple's store page (`key` for a Steam game; an Epic game is looked up by name).
    Store { key: Option<String>, query: String },
    /// Not here and no store to get it from (a game added by hand on the sender's computer).
    Missing { name: String },
}

impl PlayAction {
    /// The word for the UI's button and list ("launch" | "install" | "store" | "missing").
    pub fn kind(&self) -> &'static str {
        match self {
            PlayAction::Launch { .. } => "launch",
            PlayAction::Install { .. } => "install",
            PlayAction::Store { .. } => "store",
            PlayAction::Missing { .. } => "missing",
        }
    }

    /// The library game it acts on, if any.
    pub fn library_game(&self) -> Option<&str> {
        match self {
            PlayAction::Launch { game } | PlayAction::Install { game } => Some(game),
            _ => None,
        }
    }
}

/// Which library game an invite's game is: by id; for a game added by hand (`manual:` ids differ per computer)
/// also by name, an installed one first.
pub fn match_invite_game<'a>(game_id: &str, name: &str, library: &'a [LibGame]) -> Option<&'a LibGame> {
    if let Some(g) = library.iter().find(|g| g.id == game_id) {
        return Some(g);
    }
    if !game_id.starts_with("manual:") {
        return None;
    }
    let key = sort_name(name);
    if key.is_empty() {
        return None;
    }
    let mut same = library.iter().filter(|g| g.key == key);
    let first = same.next()?;
    Some(if first.installed { first } else { same.find(|g| g.installed).unwrap_or(first) })
}

/// What Play means here for the invite's game (`game_id` is the sender's grapple id, `name` tether's copy).
pub fn decide(game_id: &str, name: &str, library: &[LibGame]) -> PlayAction {
    match match_invite_game(game_id, name, library) {
        Some(g) if g.installed => PlayAction::Launch { game: g.id.clone() },
        Some(g) if g.store != Store::Manual => PlayAction::Install { game: g.id.clone() },
        _ if game_id.starts_with("steam:") => PlayAction::Store { key: Some(game_id.to_string()), query: name.to_string() },
        _ if game_id.starts_with("epic:") => PlayAction::Store { key: None, query: name.to_string() },
        _ => PlayAction::Missing { name: name.to_string() },
    }
}

// ---------------------------------------------------------------------------------------------- what the UI gets

/// The other person of an invite (tether_svc's `UserDto` shape).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub initial: String,
    /// `var(--avatar-N)`
    pub colour: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InviteGameDto {
    /// The sender's grapple id (`steam:<appid>`, `epic:<app name>`, `manual:<uuid>`).
    pub id: String,
    pub name: String,
}

/// One invite in the list (`invites-changed`). Never holds a `join` payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InviteDto {
    pub id: String,
    /// Josh sent it (else it was sent to him).
    pub outgoing: bool,
    /// The friend: the recipient of one Josh sent, the sender of one he got.
    pub friend: Person,
    pub game: InviteGameDto,
    /// `sent | seen | accepted | declined | cancelled | expired` on grapple's own clock (an open invite past
    /// its time is expired even before tether says so).
    pub state: String,
    /// Milliseconds since the Unix epoch.
    pub created_at: i64,
    pub expires_at: i64,
    pub answered_at: Option<i64>,
    /// Invites to Josh: what Play does here (`PlayAction::kind`).
    pub play: Option<&'static str>,
    /// The library game it matched (for its art), if any.
    pub library_game: Option<String>,
}

/// The invite a deep link opened (one Play button, pressed by the person).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenInvite {
    pub id: String,
    /// `None` while it loads (or while signed out).
    pub invite: Option<InviteDto>,
    /// "This invite isn't for this account", a network error...
    pub error: Option<String>,
}

/// `invites_snapshot` / `invites-changed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InvitesSnapshot {
    /// `off` (not signed in) | `unsupported` (this tether has no invites) | `needs_consent` (the sign-in predates
    /// them: "Allow invites") | `ready`
    pub status: &'static str,
    /// Newest first: sent and received in the last day.
    pub invites: Vec<InviteDto>,
    pub open: Option<OpenInvite>,
}

impl Default for InvitesSnapshot {
    fn default() -> Self {
        InvitesSnapshot { status: "off", invites: Vec::new(), open: None }
    }
}

/// `invite_accept`'s answer and the `invite-play` event: what grapple did (it already launched an installed game),
/// or what the UI opens (the install sheet, the store page, a note).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayOutcome {
    pub action: PlayAction,
    /// The game's name as the invite has it.
    pub name: String,
    /// A `join` text that came with the invite (a private match name and password), for the clipboard. Never
    /// logged.
    pub join: Option<String>,
    /// The launch could not start.
    pub error: Option<String>,
}

/// One friend's answer to "Send" (one invite per friend).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SendResult {
    pub friend: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapple_core::{Art, GameId};

    fn game(store: Store, id: &str, name: &str, installed: bool) -> LibGame {
        LibGame::of(&Game {
            id: GameId::new(store, id),
            store,
            name: name.into(),
            sort_name: sort_name(name),
            installed,
            install_dir: None,
            size_bytes: None,
            art: Art::default(),
            store_playtime_min: 0,
            last_played: None,
            status: None,
        })
    }

    fn library() -> Vec<LibGame> {
        vec![
            game(Store::Steam, "730", "Counter-Strike 2", true),
            game(Store::Steam, "1145350", "Hades II", false),
            game(Store::Epic, "Sugar", "Rocket League", true),
            game(Store::Epic, "Fall", "Fall Guys", false),
            game(Store::Manual, "aaaa-1", "Minecraft", true),
            game(Store::Manual, "bbbb-2", "Celeste", false),
        ]
    }

    #[test]
    fn installed_games_launch_known_ones_install_others_go_to_the_store() {
        let lib = library();
        let d = |id: &str, name: &str| decide(id, name, &lib);
        assert_eq!(d("steam:730", "Counter-Strike 2"), PlayAction::Launch { game: "steam:730".into() });
        assert_eq!(d("epic:Sugar", "Rocket League"), PlayAction::Launch { game: "epic:Sugar".into() });
        // In the library, not installed: the install sheet.
        assert_eq!(d("steam:1145350", "Hades II"), PlayAction::Install { game: "steam:1145350".into() });
        assert_eq!(d("epic:Fall", "Fall Guys"), PlayAction::Install { game: "epic:Fall".into() });
        // Not owned: the store page (Steam by its key, Epic by name), and a game added by hand: just say so.
        assert_eq!(d("steam:252950", "Rocket League"), PlayAction::Store { key: Some("steam:252950".into()), query: "Rocket League".into() });
        assert_eq!(d("epic:Other", "Alan Wake 2"), PlayAction::Store { key: None, query: "Alan Wake 2".into() });
        assert_eq!(d("manual:zzzz", "Doom (1993)"), PlayAction::Missing { name: "Doom (1993)".into() });
        // Store games match by id only: the same name under another id is not this game.
        assert!(matches!(d("steam:999", "Counter-Strike 2"), PlayAction::Store { .. }));
        assert_eq!(d("steam:730", "x").kind(), "launch");
        assert_eq!(d("steam:730", "x").library_game(), Some("steam:730"));
    }

    #[test]
    fn games_added_by_hand_match_by_name_too() {
        let lib = library();
        // The sender's manual id differs from ours: the name decides (case and punctuation folded).
        assert_eq!(decide("manual:other-id", "minecraft", &lib), PlayAction::Launch { game: "manual:aaaa-1".into() });
        assert_eq!(decide("manual:other-id", "MINECRAFT.", &lib), PlayAction::Launch { game: "manual:aaaa-1".into() });
        // A game added by hand that is not installed has no install sheet.
        assert_eq!(decide("manual:x", "Celeste", &lib), PlayAction::Missing { name: "Celeste".into() });
        // An installed copy wins over one that is not.
        let mut two = lib.clone();
        two.insert(0, game(Store::Manual, "cccc-3", "Minecraft", false));
        assert_eq!(decide("manual:x", "Minecraft", &two), PlayAction::Launch { game: "manual:aaaa-1".into() });
        assert_eq!(decide("manual:x", "!!", &lib), PlayAction::Missing { name: "!!".into() });
    }

    #[test]
    fn deep_links_are_exactly_grapple_invite_digits() {
        assert_eq!(parse_invite_link("grapple://invite/1575162397511909376").as_deref(), Some("1575162397511909376"));
        assert_eq!(parse_invite_link("grapple://invite/1").as_deref(), Some("1"));
        assert_eq!(parse_invite_link(&format!("grapple://invite/{}", "9".repeat(20))).as_deref(), Some("99999999999999999999"));
        for bad in [
            "",
            "grapple://invite/",
            "grapple://invite/123/",
            "grapple://invite/123?x=1",
            "grapple://invite/123#play",
            "grapple://invite/12a",
            "grapple://invite/-1",
            "grapple://invite/ 1",
            "grapple://invite/1 ",
            "grapple://invite/１２",
            "grapple://launch/123",
            "grapple://invite//123",
            "grapple://art/steam%3A730/cover",
            "GRAPPLE://invite/123",
            "grapple:invite/123",
            "https://invite/123",
            "grapple://invite/123\n",
            &format!("grapple://invite/{}", "9".repeat(21)),
        ] {
            assert_eq!(parse_invite_link(bad), None, "{bad:?}");
        }
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(invite_link_arg(&args(&["grapple://invite/42"])).as_deref(), Some("42"));
        assert_eq!(invite_link_arg(&args(&["--hidden", "grapple://invite/42"])).as_deref(), Some("42"));
        assert_eq!(invite_link_arg(&args(&["--launch", "steam:730"])), None);
        assert_eq!(invite_link_arg(&args(&["grapple://invite/42/play"])), None);
        assert_eq!(invite_link_arg(&[]), None);
    }
}
