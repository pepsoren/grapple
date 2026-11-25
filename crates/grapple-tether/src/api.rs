//! The REST routes an app token may use (contract section 3).

use crate::config::TetherConfig;
use crate::error::{Error, Result};
use crate::invites::{
    AcceptedInvite, GameInvite, INVITE_EXPIRES_MAX_SECS, INVITE_EXPIRES_MIN_SECS, InviteGame, InviteJoin, InvitesFrom,
    valid_snowflake,
};
use crate::model::{GameImage, TetherUser};
use crate::tokens::TokenManager;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Largest icon tether takes.
pub const ICON_MAX_BYTES: usize = 64 * 1024;

pub struct Api {
    config: TetherConfig,
    http: reqwest::Client,
    tokens: Arc<TokenManager>,
}

#[derive(Deserialize)]
struct GatewayInfo {
    url: String,
}

#[derive(Deserialize)]
struct Relationship {
    #[serde(rename = "type")]
    kind: u8,
    user: TetherUser,
}

impl Api {
    pub(crate) fn new(
        config: TetherConfig,
        http: reqwest::Client,
        tokens: Arc<TokenManager>,
    ) -> Self {
        Api {
            config,
            http,
            tokens,
        }
    }

    pub fn config(&self) -> &TetherConfig {
        &self.config
    }

    /// The HTTP client for public routes (no token is ever added to it).
    pub(crate) fn public_http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Send with the app token; on a 401, refresh once and try again.
    async fn send(
        &self,
        build: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        let mut token = self.tokens.access_token().await?;
        for attempt in 0..2 {
            let res = build(&self.http).bearer_auth(token.expose()).send().await?;
            if res.status() != reqwest::StatusCode::UNAUTHORIZED || attempt == 1 {
                if res.status() == reqwest::StatusCode::UNAUTHORIZED {
                    self.tokens.forget().await?;
                    return Err(Error::SignedOut);
                }
                return Ok(res);
            }
            token = self.tokens.refresh_after_rejection(&token).await?;
        }
        unreachable!("the loop returns on its second attempt")
    }

    /// Where the gateway is (no token needed).
    pub async fn gateway_url(&self) -> Result<String> {
        let res = self.http.get(self.config.api("/gateway")).send().await?;
        if !res.status().is_success() {
            return Err(Error::Status(res.status().as_u16()));
        }
        let info: GatewayInfo = res.json().await?;
        if !(info.url.starts_with("wss://") || info.url.starts_with("ws://")) {
            return Err(Error::Protocol(
                "the gateway address is not a WebSocket address".into(),
            ));
        }
        Ok(info.url)
    }

    /// The signed-in account (`identify`).
    pub async fn me(&self) -> Result<TetherUser> {
        let res = self.send(|h| h.get(self.config.api("/users/@me"))).await?;
        if !res.status().is_success() {
            return Err(Error::Status(res.status().as_u16()));
        }
        Ok(res.json().await?)
    }

    /// The friends list (`friends.read`).
    pub async fn friends(&self) -> Result<Vec<TetherUser>> {
        let res = self
            .send(|h| h.get(self.config.api("/users/@me/relationships")))
            .await?;
        if !res.status().is_success() {
            return Err(Error::Status(res.status().as_u16()));
        }
        let list: Vec<Relationship> = res.json().await?;
        Ok(list
            .into_iter()
            .filter(|r| r.kind == 1)
            .map(|r| r.user)
            .collect())
    }

    /// Make sure tether has this PNG; returns its id (lowercase hex SHA-256) for `icon:<id>`.
    pub async fn ensure_icon(&self, png: &[u8]) -> Result<String> {
        if png.len() > ICON_MAX_BYTES {
            return Err(Error::Icon(format!(
                "{} bytes is over the 64 KB tether takes",
                png.len()
            )));
        }
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(Error::Icon("not a PNG".into()));
        }
        let sha: String = Sha256::digest(png)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let url = self.config.api(&format!("/game-icons/{sha}"));
        let head = self.send(|h| h.head(&url)).await?;
        if head.status().is_success() {
            return Ok(sha);
        }
        if head.status() != reqwest::StatusCode::NOT_FOUND {
            return Err(Error::Status(head.status().as_u16()));
        }
        let body = png.to_vec();
        let put = self
            .send(|h| {
                h.put(&url)
                    .header("content-type", "image/png")
                    .body(body.clone())
            })
            .await?;
        if put.status().is_success() {
            Ok(sha)
        } else {
            Err(Error::Status(put.status().as_u16()))
        }
    }

    /// A game picture (128 px WebP), or `None` when tether has none.
    pub async fn game_icon(&self, image: &GameImage) -> Result<Option<Vec<u8>>> {
        let url = self.config.api(&image.path());
        let res = self.send(|h| h.get(&url)).await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(Error::Status(res.status().as_u16()));
        }
        Ok(Some(res.bytes().await?.to_vec()))
    }

    // ---- game invites (contract section 7; every route needs the `invites` scope)

    /// Does this tether have game invites? One GET of an invite route with no token: a tether with
    /// them answers 401, an older one 404 (and refuses a sign-in that asks for `invites`).
    pub async fn invites_supported(&self) -> bool {
        let answer = self
            .http
            .get(self.config.api("/users/@me/game-invite-settings"))
            .timeout(std::time::Duration::from_secs(8))
            .send()
            .await;
        matches!(answer.map(|r| r.status().as_u16()), Ok(s) if !matches!(s, 404 | 405 | 410 | 501))
    }

    /// A refused answer as the contract's error (never with a token or a `join` payload in it).
    async fn refused(&self, res: reqwest::Response) -> Error {
        #[derive(Deserialize)]
        struct Body {
            code: u32,
            #[serde(default)]
            message: String,
            #[serde(default)]
            retry_after: Option<f64>,
        }
        let status = res.status().as_u16();
        let header_wait = res
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body: Option<Body> = res.json().await.ok();
        match (status, body.as_ref().map(|b| b.code)) {
            (403, Some(50001)) => {
                // The same code means "no `invites` scope" and "the wrong side of this invite".
                if self.tokens.scopes().await.unwrap_or_default().iter().any(|s| s == "invites") {
                    Error::NotAllowed
                } else {
                    Error::NeedsConsent
                }
            }
            (403, Some(50007)) => Error::CannotInvite,
            (404, Some(10013)) => Error::UnknownUser,
            (404, Some(90270)) => Error::UnknownInvite,
            (400, Some(90271)) => Error::InviteClosed,
            (429, _) => Error::RateLimited {
                retry_after_secs: body
                    .as_ref()
                    .and_then(|b| b.retry_after)
                    .map(|s| s.ceil().max(1.0) as u64)
                    .or(header_wait)
                    .unwrap_or(60),
            },
            (400, Some(50035)) => Error::Invalid(body.map(|b| b.message).filter(|m| !m.is_empty()).unwrap_or_else(|| "Invalid Form Body".into())),
            _ => Error::Status(status),
        }
    }

    async fn invite_json<T: serde::de::DeserializeOwned>(&self, res: reqwest::Response) -> Result<T> {
        if res.status().is_success() {
            Ok(res.json().await?)
        } else {
            Err(self.refused(res).await)
        }
    }

    fn invite_path(id: &str, action: &str) -> Result<String> {
        // Only digits ever go into the path.
        if !valid_snowflake(id) {
            return Err(Error::UnknownInvite);
        }
        Ok(format!("/users/@me/game-invites/{id}{action}"))
    }

    /// Invite a friend to a game (one invite per friend). `expires_in_secs`: 60 to 3600 (tether's
    /// default, 15 minutes, when `None`). tether posts it in their DM as Josh, "via grapple".
    pub async fn send_invite(
        &self,
        recipient_id: &str,
        game: &InviteGame,
        expires_in_secs: Option<u32>,
    ) -> Result<GameInvite> {
        if !valid_snowflake(recipient_id) {
            return Err(Error::UnknownUser);
        }
        let mut body = json!({ "recipient_id": recipient_id, "game": { "id": game.id, "name": game.name } });
        if let Some(image) = &game.image {
            body["game"]["image"] = json!(image);
        }
        if let Some(secs) = expires_in_secs {
            body["expires_in"] = json!(secs.clamp(INVITE_EXPIRES_MIN_SECS, INVITE_EXPIRES_MAX_SECS));
        }
        let url = self.config.api("/users/@me/game-invites");
        let res = self.send(|h| h.post(&url).json(&body)).await?;
        self.invite_json(res).await
    }

    /// The invites Josh sent or got in the last day, newest first (`limit` 1 to 100).
    pub async fn invites(&self, limit: u8) -> Result<Vec<GameInvite>> {
        let url = self.config.api(&format!("/users/@me/game-invites?limit={}", limit.clamp(1, 100)));
        let res = self.send(|h| h.get(&url)).await?;
        self.invite_json(res).await
    }

    /// One invite. Asked by its recipient, also the `join` payload while it is open (or accepted and
    /// not past `expires_at`).
    pub async fn invite(&self, id: &str) -> Result<(GameInvite, Option<InviteJoin>)> {
        #[derive(Deserialize)]
        struct WithJoin {
            #[serde(flatten)]
            invite: GameInvite,
            #[serde(default)]
            join: serde_json::Value,
        }
        let url = self.config.api(&Self::invite_path(id, "")?);
        let res = self.send(|h| h.get(&url)).await?;
        let w: WithJoin = self.invite_json(res).await?;
        let join = InviteJoin::from_value(&w.join);
        Ok((w.invite, join))
    }

    /// The action routes take no body (and no `Content-Type`).
    async fn act<T: serde::de::DeserializeOwned>(&self, id: &str, action: &str) -> Result<T> {
        let url = self.config.api(&Self::invite_path(id, action)?);
        let res = self.send(|h| h.post(&url)).await?;
        self.invite_json(res).await
    }

    /// The recipient's Grapple showed it (`sent` → `seen`).
    pub async fn invite_seen(&self, id: &str) -> Result<GameInvite> {
        self.act(id, "/seen").await
    }

    /// The recipient pressed Play in Grapple. tether sends no `GAME_INVITE_PLAY` for this: Grapple acts
    /// itself. Accepting an accepted invite again before `expires_at` is fine (Play again).
    pub async fn accept_invite(&self, id: &str) -> Result<AcceptedInvite> {
        self.act(id, "/accept").await
    }

    pub async fn decline_invite(&self, id: &str) -> Result<GameInvite> {
        self.act(id, "/decline").await
    }

    /// The sender takes it back.
    pub async fn cancel_invite(&self, id: &str) -> Result<GameInvite> {
        self.act(id, "/cancel").await
    }

    /// "Who can invite me".
    pub async fn invite_settings(&self) -> Result<InvitesFrom> {
        #[derive(Deserialize)]
        struct S {
            from: InvitesFrom,
        }
        let url = self.config.api("/users/@me/game-invite-settings");
        let res = self.send(|h| h.get(&url)).await?;
        Ok(self.invite_json::<S>(res).await?.from)
    }

    pub async fn set_invite_settings(&self, from: InvitesFrom) -> Result<InvitesFrom> {
        #[derive(Deserialize)]
        struct S {
            from: InvitesFrom,
        }
        let url = self.config.api("/users/@me/game-invite-settings");
        let body = json!({ "from": from });
        let res = self.send(|h| h.put(&url).json(&body)).await?;
        Ok(self.invite_json::<S>(res).await?.from)
    }
}
