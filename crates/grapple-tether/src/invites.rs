//! Game invites ("come play with me", contract section 7): the invite as tether gives it, the game
//! Grapple sends, the optional `join` payload, and "Who can invite me".
//!
//! Privacy: a `join` payload (a private match name and password, say) lives in [`InviteJoin`], whose
//! value is a [`Secret`]: `Debug` never shows it and nothing here logs it.

use crate::error::{Error, Result};
use crate::model::GameImage;
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How long an invite stays open when Grapple names no time (tether's default too): 15 minutes.
pub const INVITE_EXPIRES_DEFAULT_SECS: u32 = 15 * 60;
/// What tether accepts for `expires_in`.
pub const INVITE_EXPIRES_MIN_SECS: u32 = 60;
pub const INVITE_EXPIRES_MAX_SECS: u32 = 60 * 60;
/// tether's game name limit (characters, once trimmed).
const NAME_MAX_CHARS: usize = 128;

/// The state tether gives an invite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InviteState {
    /// Posted, not yet shown to the friend.
    Sent,
    /// The friend's tether (or Grapple) showed it.
    Seen,
    /// The friend pressed Play ("Joined" for the sender).
    Accepted,
    Declined,
    /// The sender took it back.
    Cancelled,
    /// `sent` or `seen` past `expires_at`.
    Expired,
    /// A state a newer tether added; treated as ended.
    #[serde(other)]
    Unknown,
}

impl InviteState {
    pub fn as_str(self) -> &'static str {
        match self {
            InviteState::Sent => "sent",
            InviteState::Seen => "seen",
            InviteState::Accepted => "accepted",
            InviteState::Declined => "declined",
            InviteState::Cancelled => "cancelled",
            InviteState::Expired => "expired",
            InviteState::Unknown => "unknown",
        }
    }
}

/// The game of an invite: Grapple's own id (`steam:<appid>`, `epic:<app name>`, `manual:<uuid>`), the
/// name, and the picture (`steam:<appid>` / `icon:<sha256>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteGame {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub image: Option<String>,
}

fn valid_epic_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-')
}

/// tether's `^(steam:\d{1,10}|epic:[A-Za-z0-9._:-]{1,128}|manual:[A-Za-z0-9-]{1,64})$`.
pub fn valid_invite_game_id(id: &str) -> bool {
    if let Some(n) = id.strip_prefix("steam:") {
        (1..=10).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit())
    } else if let Some(n) = id.strip_prefix("epic:") {
        (1..=128).contains(&n.len()) && n.bytes().all(valid_epic_char)
    } else if let Some(n) = id.strip_prefix("manual:") {
        (1..=64).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    } else {
        false
    }
}

/// A snowflake as tether writes it: 1 to 20 decimal digits (also the `<id>` of `grapple://invite/<id>`).
pub fn valid_snowflake(id: &str) -> bool {
    (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

impl InviteGame {
    /// The game as `POST /users/@me/game-invites` takes it, checked like tether checks it (so a bad one
    /// never reaches the network): the id's shape, the name trimmed (at most 128 characters), the picture.
    pub fn new(id: &str, name: &str, image: Option<&str>) -> Result<InviteGame> {
        if !valid_invite_game_id(id) {
            return Err(Error::Invalid("this game can't be sent in an invite".into()));
        }
        let name: String = name.trim().chars().take(NAME_MAX_CHARS).collect();
        if name.is_empty() {
            return Err(Error::Invalid("a game needs a name".into()));
        }
        let image = image.filter(|i| GameImage::parse(i).is_some()).map(str::to_string);
        Ok(InviteGame { id: id.to_string(), name, image })
    }
}

/// An invite (`APIGameInvite`). Times are tether's ISO 8601 strings; the `*_ms` methods read them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameInvite {
    pub id: String,
    pub channel_id: String,
    pub message_id: String,
    pub sender_id: String,
    pub recipient_id: String,
    pub game: InviteGame,
    pub state: InviteState,
    /// A `join` payload came with it (the payload itself is never in this object).
    #[serde(default)]
    pub joinable: bool,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub seen_at: Option<String>,
    /// When it was accepted, declined or cancelled.
    #[serde(default)]
    pub answered_at: Option<String>,
}

impl GameInvite {
    pub fn created_ms(&self) -> Option<i64> {
        parse_iso_ms(&self.created_at)
    }

    pub fn expires_ms(&self) -> Option<i64> {
        parse_iso_ms(&self.expires_at)
    }

    pub fn answered_ms(&self) -> Option<i64> {
        self.answered_at.as_deref().and_then(parse_iso_ms)
    }

    /// The state to show at `now_ms`: an open invite past `expires_at` is expired, on Grapple's own clock,
    /// even before tether's minute sweep says so. `accepted` stays `accepted`.
    pub fn state_at(&self, now_ms: i64) -> InviteState {
        match self.state {
            InviteState::Sent | InviteState::Seen if self.expires_ms().is_none_or(|t| t <= now_ms) => InviteState::Expired,
            s => s,
        }
    }

    /// Can the friend still answer it (sent or seen, before `expires_at`)?
    pub fn is_open(&self, now_ms: i64) -> bool {
        matches!(self.state_at(now_ms), InviteState::Sent | InviteState::Seen)
    }
}

/// `POST .../<id>/accept`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AcceptedInvite {
    pub invite: GameInvite,
    /// Always false when Grapple accepted (with its app token): Grapple acts itself.
    #[serde(default)]
    pub app_notified: bool,
}

/// What comes with an invite to join the sender (only to the recipient's Grapple). v1 knows `text`;
/// kinds a newer tether adds are ignored (treated as no payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteJoin {
    /// Free text, e.g. "Private match: grapple / hunter2": copied to the clipboard on Play.
    Text(Secret),
}

impl InviteJoin {
    /// From the wire's `{"kind": "text", "value": "..."}`; `None` for a missing, empty or unknown one.
    pub fn from_value(v: &Value) -> Option<InviteJoin> {
        match v.get("kind")?.as_str()? {
            "text" => {
                let value = v.get("value")?.as_str()?.trim();
                (!value.is_empty() && !value.chars().any(char::is_control)).then(|| InviteJoin::Text(Secret::new(value)))
            }
            _ => None,
        }
    }
}

/// "Who can invite me" (enforced by tether).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvitesFrom {
    Friends,
    Nobody,
}

/// Milliseconds since the Unix epoch from an ISO 8601 UTC time as tether writes it
/// (`2026-11-25T18:15:00.000Z`; the fraction and `Z`/`+00:00` are optional). `None` for anything else.
pub fn parse_iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once(['T', 't', ' '])?;
    let mut d = date.splitn(3, '-');
    let (y, mo, da): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time = time
        .strip_suffix(['Z', 'z'])
        .or_else(|| time.strip_suffix("+00:00"))
        .or_else(|| time.strip_suffix("+0000"))?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.splitn(3, ':');
    let (h, mi, se): (i64, i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || se > 60 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ms: i64 = format!("{frac:0<3}")[..3].parse().ok()?;
    Some((days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + se) * 1000 + ms)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn invite(state: &str, expires_at: &str) -> GameInvite {
        serde_json::from_value(json!({
            "id": "1575162397511909376", "channel_id": "1", "message_id": "1575162397511909376",
            "sender_id": "100", "recipient_id": "200",
            "game": { "id": "steam:252950", "name": "Rocket League", "image": "steam:252950" },
            "state": state, "joinable": false,
            "created_at": "2026-11-25T18:00:00.000Z", "expires_at": expires_at,
            "seen_at": null, "answered_at": null, "some_future_field": 1
        }))
        .unwrap()
    }

    #[test]
    fn reads_tethers_iso_times() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_iso_ms("2026-11-25T18:15:00.000Z"), Some(1_795_630_500_000));
        assert_eq!(parse_iso_ms("2026-11-25T18:15:00Z"), Some(1_795_630_500_000));
        assert_eq!(parse_iso_ms("2026-11-25T18:15:00.5Z"), Some(1_795_630_500_500));
        assert_eq!(parse_iso_ms("2000-02-29T12:00:00+00:00"), Some(951_825_600_000));
        for bad in ["", "yesterday", "2026-11-25", "2026-11-25T18:15:00", "2026-13-01T00:00:00Z", "2026-11-25T18:15:00+02:00", "2026-11-25T18:15:00.x1Z"] {
            assert_eq!(parse_iso_ms(bad), None, "{bad}");
        }
    }

    #[test]
    fn an_open_invite_past_its_time_is_expired_on_our_own_clock() {
        let expires = "2026-11-25T18:15:00.000Z";
        let at = parse_iso_ms(expires).unwrap();
        let sent = invite("sent", expires);
        assert_eq!(sent.state_at(at - 1), InviteState::Sent);
        assert!(sent.is_open(at - 1));
        assert_eq!(sent.state_at(at), InviteState::Expired);
        assert!(!sent.is_open(at));
        assert_eq!(invite("seen", expires).state_at(at + 1), InviteState::Expired);
        // Accepted stays accepted; ended ones never change; a newer tether's state counts as ended.
        assert_eq!(invite("accepted", expires).state_at(at + 60_000), InviteState::Accepted);
        assert_eq!(invite("declined", expires).state_at(at - 1), InviteState::Declined);
        let odd = invite("snoozed", expires);
        assert_eq!(odd.state, InviteState::Unknown);
        assert!(!odd.is_open(at - 1));
        // An unreadable expiry is not open.
        assert!(!invite("sent", "soon").is_open(0));
        assert_eq!(sent.created_ms(), Some(at - 15 * 60_000));
    }

    #[test]
    fn game_ids_and_names_are_checked_like_tether() {
        assert!(valid_invite_game_id("steam:252950"));
        assert!(valid_invite_game_id("epic:Sugar"));
        assert!(valid_invite_game_id("epic:ns:item.1_a-b"));
        assert!(valid_invite_game_id("manual:2b1d0c9e-1f2a-4c3b-9d8e-7f6a5b4c3d2e"));
        for bad in ["steam:", "steam:12345678901", "steam:7a", "epic:a b", "manual:x_y", "gog:1", "steam:730/"] {
            assert!(!valid_invite_game_id(bad), "{bad}");
        }
        let g = InviteGame::new("steam:730", "  Counter-Strike 2 ", Some("steam:730")).unwrap();
        assert_eq!((g.name.as_str(), g.image.as_deref()), ("Counter-Strike 2", Some("steam:730")));
        // A picture tether would refuse is left out rather than failing the invite.
        assert_eq!(InviteGame::new("epic:Sugar", "Rocket League", Some("http://x")).unwrap().image, None);
        assert_eq!(InviteGame::new("manual:a", &"x".repeat(300), None).unwrap().name.chars().count(), 128);
        assert!(matches!(InviteGame::new("manual:a", "   ", None), Err(Error::Invalid(_))));
        assert!(valid_snowflake("1575162397511909376") && !valid_snowflake("") && !valid_snowflake("12a") && !valid_snowflake(&"1".repeat(21)));
    }

    #[test]
    fn join_payloads_keep_text_only_and_never_show_it() {
        let j = InviteJoin::from_value(&json!({ "kind": "text", "value": " Private match: grapple / hunter2 " })).unwrap();
        let InviteJoin::Text(v) = &j;
        assert_eq!(v.expose(), "Private match: grapple / hunter2");
        assert!(!format!("{j:?}").contains("hunter2"));
        assert_eq!(InviteJoin::from_value(&json!({ "kind": "steam_lobby", "value": "1" })), None);
        assert_eq!(InviteJoin::from_value(&json!({ "kind": "text", "value": "  " })), None);
        assert_eq!(InviteJoin::from_value(&json!({ "kind": "text", "value": "a\u{7}b" })), None);
        assert_eq!(InviteJoin::from_value(&Value::Null), None);
    }
}
