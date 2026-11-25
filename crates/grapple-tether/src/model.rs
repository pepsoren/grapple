//! What Grapple sends and receives, in Grapple's terms (the wire shapes are in the contract).

use crate::config::TetherConfig;
use crate::invites::{GameInvite, InviteJoin};
use serde::{Deserialize, Serialize};

/// A tether account as other people see it (no email, no settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TetherUser {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
}

impl TetherUser {
    pub fn display_name(&self) -> &str {
        self.global_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.username)
    }

    /// The avatar picture (public; no token needed), or `None` (draw the initial).
    pub fn avatar_url(&self, config: &TetherConfig) -> Option<String> {
        self.avatar
            .as_ref()
            .map(|hash| config.api(&format!("/avatars/{}/{hash}", self.id)))
    }
}

/// The picture to show with a game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameArt {
    None,
    /// A Steam app id: tether fetches Steam's picture itself (`steam:<appid>`).
    Steam(u32),
    /// A PNG (at most 64 KB) for an Epic or manual game: uploaded once, then `icon:<sha256>`.
    IconPng(Vec<u8>),
}

/// The game Josh is playing, as Grapple publishes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameActivity {
    pub name: String,
    /// When the game started, milliseconds since the Unix epoch.
    pub started_at_ms: i64,
    pub art: GameArt,
}

impl GameActivity {
    pub fn steam(name: impl Into<String>, app_id: u32, started_at_ms: i64) -> Self {
        GameActivity {
            name: name.into(),
            started_at_ms,
            art: GameArt::Steam(app_id),
        }
    }

    pub fn with_icon(name: impl Into<String>, png: Vec<u8>, started_at_ms: i64) -> Self {
        GameActivity {
            name: name.into(),
            started_at_ms,
            art: GameArt::IconPng(png),
        }
    }

    pub fn plain(name: impl Into<String>, started_at_ms: i64) -> Self {
        GameActivity {
            name: name.into(),
            started_at_ms,
            art: GameArt::None,
        }
    }
}

/// A friend's game picture as tether names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameImage {
    Steam(String),
    Icon(String),
}

impl GameImage {
    pub fn parse(value: &str) -> Option<Self> {
        if let Some(id) = value.strip_prefix("steam:") {
            (!id.is_empty() && id.len() <= 10 && id.bytes().all(|b| b.is_ascii_digit()))
                .then(|| GameImage::Steam(id.to_string()))
        } else if let Some(sha) = value.strip_prefix("icon:") {
            (sha.len() == 64 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                .then(|| GameImage::Icon(sha.to_string()))
        } else {
            None
        }
    }

    /// The picture's API path (fetch it with the app token: `Api::game_icon`).
    pub fn path(&self) -> String {
        match self {
            GameImage::Steam(id) => format!("/game-icons/steam/{id}"),
            GameImage::Icon(sha) => format!("/game-icons/{sha}"),
        }
    }
}

/// What a friend is playing, and since when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendGame {
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub since_ms: i64,
    pub image: Option<GameImage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Online,
    Idle,
    Dnd,
    Offline,
}

impl Status {
    pub(crate) fn parse(value: &str) -> Status {
        match value {
            "online" => Status::Online,
            "idle" => Status::Idle,
            "dnd" => Status::Dnd,
            _ => Status::Offline,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FriendPresence {
    pub user: TetherUser,
    pub status: Status,
    /// `None` when not playing (always `None` while offline).
    pub game: Option<FriendGame>,
}

/// What the gateway task reports.
#[derive(Debug, Clone, PartialEq)]
pub enum TetherEvent {
    /// A fresh session: who Josh is and every friend (offline ones included).
    Ready {
        me: TetherUser,
        friends: Vec<FriendPresence>,
    },
    /// A dropped connection came back without losing anything.
    Resumed,
    /// A friend's presence changed, or a new friend.
    Friend(FriendPresence),
    /// Someone is no longer a friend.
    FriendRemoved { user_id: String },
    /// The connection dropped; the task is reconnecting.
    Disconnected,
    /// The sign-in is gone (revoked in tether, or lapsed). The task has stopped; the tokens are
    /// already forgotten.
    SignedOut,
    /// Something that retrying cannot fix (e.g. the account must confirm its email). The task
    /// has stopped.
    Fatal(String),
    /// `GAME_INVITE_CREATE`: an invite sent by Josh (from any computer) or to him.
    InviteCreated(GameInvite),
    /// `GAME_INVITE_UPDATE`: an invite's state changed (replace the stored copy by id).
    InviteUpdated(GameInvite),
    /// `GAME_INVITE_PLAY`: Josh pressed Play on an invite's card in tether (it is already accepted;
    /// do not accept it again). `join` only when a payload came with it.
    InvitePlay {
        invite: GameInvite,
        join: Option<InviteJoin>,
    },
}
