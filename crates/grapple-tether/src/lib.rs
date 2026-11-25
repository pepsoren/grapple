//! tether client for Grapple: "Sign in with tether", publishing what Josh plays, and friends'
//! presence. The wire contract is `docs/tether-contract.md`; this crate follows it exactly.
//!
//! - [`Tether`] holds the instance ([`TetherConfig`]) and the tokens ([`TokenStore`], the Secret
//!   Service keyring in the app, a [`MemoryStore`] in tests). [`Tether::sign_in`] runs the browser
//!   flow (loopback listener, PKCE S256, state check), [`Tether::sign_out`] revokes and forgets.
//! - [`Tether::connect`] starts the gateway task and returns a [`GatewayHandle`]:
//!   [`GatewayHandle::set_game`] publishes the game (uploading a non-Steam icon first),
//!   [`GatewayHandle::subscribe`] streams [`TetherEvent`]s (who is playing what since when).
//!
//! Privacy: tokens, codes, verifiers and `state` live in [`Secret`] (its `Debug` prints `***`),
//! are never logged, never written to plain files and never put in error messages.

mod api;
mod avatar;
mod client;
mod config;
mod error;
mod gateway;
mod invites;
mod model;
pub mod oauth;
pub mod probe;
mod secret;
mod store;
mod tokens;

pub use api::Api;
pub use avatar::{AVATAR_MAX_BYTES, AvatarImage, ImageKind, valid_avatar_hash, valid_user_id};
pub use client::Tether;
pub use config::{DEFAULT_INSTANCE_URL, INSTANCE_SETTING_KEY, TetherConfig};
pub use error::{Error, Result};
pub use gateway::{GatewayHandle, GatewayOptions};
pub use invites::{
    AcceptedInvite, GameInvite, INVITE_EXPIRES_DEFAULT_SECS, INVITE_EXPIRES_MAX_SECS, INVITE_EXPIRES_MIN_SECS, InviteGame,
    InviteJoin, InviteState, InvitesFrom, parse_iso_ms, valid_invite_game_id, valid_snowflake,
};
pub use model::{
    FriendGame, FriendPresence, GameActivity, GameArt, GameImage, Status, TetherEvent, TetherUser,
};
pub use oauth::{Browser, XdgOpen};
pub use probe::Probe;
pub use secret::Secret;
pub use store::{KEYRING_SERVICE, KeyringStore, MemoryStore, StoredTokens, TokenStore};
