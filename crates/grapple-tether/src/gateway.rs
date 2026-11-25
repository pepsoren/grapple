//! The gateway task (contract section 4): connect, IDENTIFY with the app token, heartbeat, resume
//! or re-identify with backoff, publish the game (op 3), and keep the friends' presence.

use crate::api::Api;
use crate::error::{Error, Result};
use crate::invites::{GameInvite, InviteJoin};
use crate::model::{
    FriendGame, FriendPresence, GameActivity, GameArt, GameImage, Status, TetherEvent, TetherUser,
};
use crate::secret::Secret;
use crate::tokens::{TokenManager, now_ms};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

const OP_DISPATCH: u64 = 0;
const OP_HEARTBEAT: u64 = 1;
const OP_IDENTIFY: u64 = 2;
const OP_PRESENCE_UPDATE: u64 = 3;
const OP_RESUME: u64 = 6;
const OP_RECONNECT: u64 = 7;
const OP_INVALID_SESSION: u64 = 9;
const OP_HELLO: u64 = 10;
const OP_HEARTBEAT_ACK: u64 = 11;

/// How often a connected session checks whether the tokens need a refresh.
const TOKEN_CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

/// tether's game name limit.
const NAME_MAX_CHARS: usize = 128;

/// Knobs (tests make them short).
#[derive(Debug, Clone)]
pub struct GatewayOptions {
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// How long to wait after INVALID_SESSION before identifying again (1 to 5 s in the contract).
    pub invalid_session_delay: Duration,
    pub connect_timeout: Duration,
}

impl Default for GatewayOptions {
    fn default() -> Self {
        GatewayOptions {
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            invalid_session_delay: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

/// The game as it goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WireGame {
    name: String,
    start: i64,
    image: Option<String>,
}

impl WireGame {
    fn activity(&self) -> Value {
        let mut a = json!({ "name": self.name, "type": 0, "timestamps": { "start": self.start } });
        if let Some(image) = &self.image {
            a["assets"] = json!({ "large_image": image });
        }
        a
    }
}

enum Command {
    SetGame(Option<WireGame>),
    Shutdown(oneshot::Sender<()>),
}

type Friends = Arc<RwLock<HashMap<String, FriendPresence>>>;
/// What the last READY said the sign-in may do (`None` before the first READY).
type Scopes = Arc<RwLock<Option<Vec<String>>>>;

/// The running gateway connection. Dropping it stops the task too.
pub struct GatewayHandle {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<TetherEvent>,
    friends: Friends,
    scopes: Scopes,
    api: Arc<Api>,
    task: Option<JoinHandle<()>>,
}

impl GatewayHandle {
    /// Publish (or with `None` clear) what Josh is playing. A non-Steam icon is uploaded first
    /// (`icon:<sha256>`); if that fails the game goes out without a picture.
    pub async fn set_game(&self, game: Option<GameActivity>) -> Result<()> {
        let wire = match game {
            None => None,
            Some(g) => Some(self.to_wire(g).await?),
        };
        self.commands
            .send(Command::SetGame(wire))
            .await
            .map_err(|_| Error::Gateway("the gateway task has stopped".into()))
    }

    async fn to_wire(&self, game: GameActivity) -> Result<WireGame> {
        let name: String = game.name.trim().chars().take(NAME_MAX_CHARS).collect();
        if name.is_empty() {
            return Err(Error::Protocol("a game needs a name".into()));
        }
        let image = match game.art {
            GameArt::None => None,
            GameArt::Steam(id) => Some(format!("steam:{id}")),
            GameArt::IconPng(png) => match self.api.ensure_icon(&png).await {
                Ok(sha) => Some(format!("icon:{sha}")),
                Err(err @ (Error::SignedOut | Error::NotSignedIn)) => return Err(err),
                Err(err) => {
                    tracing::warn!(error = %err, "game icon upload failed; sending the game without it");
                    None
                }
            },
        };
        // tether refuses a start in the future (beyond a minute) or more than 7 days ago.
        let start = game
            .started_at_ms
            .min(now_ms())
            .max(now_ms() - 7 * 24 * 3600 * 1000 + 60_000);
        Ok(WireGame { name, start, image })
    }

    /// Every event from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<TetherEvent> {
        self.events.subscribe()
    }

    /// The friends as last heard (empty before the first READY).
    pub fn friends(&self) -> Vec<FriendPresence> {
        let mut list: Vec<_> = self.friends.read().unwrap().values().cloned().collect();
        list.sort_by(|a, b| {
            a.user
                .display_name()
                .to_lowercase()
                .cmp(&b.user.display_name().to_lowercase())
        });
        list
    }

    /// The scopes of the current session, from its READY (`None` until the first one). Without
    /// `invites` the session gets no game invite events.
    pub fn scopes(&self) -> Option<Vec<String>> {
        self.scopes.read().unwrap().clone()
    }

    /// Close the connection cleanly (tether forgets the session and its game) and stop the task.
    pub async fn shutdown(mut self) {
        let (tx, rx) = oneshot::channel();
        if self.commands.send(Command::Shutdown(tx)).await.is_ok() {
            let _ = timeout(Duration::from_secs(5), rx).await;
        }
        if let Some(task) = self.task.take() {
            let _ = timeout(Duration::from_secs(5), task).await;
        }
    }
}

impl Drop for GatewayHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(crate) fn spawn(
    api: Arc<Api>,
    tokens: Arc<TokenManager>,
    options: GatewayOptions,
) -> GatewayHandle {
    let (commands, rx) = mpsc::channel(32);
    let (events, _) = broadcast::channel(256);
    let friends: Friends = Arc::default();
    let scopes: Scopes = Arc::default();
    let runner = Runner {
        api: api.clone(),
        tokens,
        options,
        events: events.clone(),
        friends: friends.clone(),
        scopes: scopes.clone(),
        commands: rx,
        game: None,
        published: None,
        resume: None,
        seq: None,
        game_allowed: true,
        backoff: Duration::ZERO,
    };
    let task = tokio::spawn(runner.run());
    GatewayHandle {
        commands,
        events,
        friends,
        scopes,
        api,
        task: Some(task),
    }
}

struct Resume {
    session_id: String,
    url: String,
}

enum Outcome {
    Shutdown,
    /// Reconnect; resume if a session is kept.
    Reconnect,
    /// tether refused the token (4004): refresh, then identify.
    AuthFailed(Secret),
    Fatal(String),
}

struct Runner {
    api: Arc<Api>,
    tokens: Arc<TokenManager>,
    options: GatewayOptions,
    events: broadcast::Sender<TetherEvent>,
    friends: Friends,
    scopes: Scopes,
    commands: mpsc::Receiver<Command>,
    /// What Grapple wants published.
    game: Option<WireGame>,
    /// What tether's session has (a new session starts with none; a resumed one keeps it).
    published: Option<WireGame>,
    resume: Option<Resume>,
    seq: Option<u64>,
    /// False when the sign-in lacks `presence.game` (op 3 would close the socket).
    game_allowed: bool,
    backoff: Duration,
}

fn jitter(d: Duration) -> Duration {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    // 80% to 120%.
    let f = 0.8 + (u16::from_le_bytes(b) as f64 / u16::MAX as f64) * 0.4;
    d.mul_f64(f)
}

fn random_fraction() -> f64 {
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    u16::from_le_bytes(b) as f64 / u16::MAX as f64
}

impl Runner {
    fn emit(&self, event: TetherEvent) {
        let _ = self.events.send(event);
    }

    async fn run(mut self) {
        let mut gateway_url: Option<String> = None;
        loop {
            let url = match (&self.resume, &gateway_url) {
                (Some(r), _) => r.url.clone(),
                (None, Some(u)) => u.clone(),
                (None, None) => match self.api.gateway_url().await {
                    Ok(u) => {
                        gateway_url = Some(u.clone());
                        u
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "could not find the tether gateway");
                        if !self.wait_backoff().await {
                            return;
                        }
                        continue;
                    }
                },
            };
            let token = match self.tokens.access_token().await {
                Ok(t) => t,
                Err(Error::SignedOut | Error::NotSignedIn) => {
                    return self.emit(TetherEvent::SignedOut);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "no tether token right now");
                    if !self.wait_backoff().await {
                        return;
                    }
                    continue;
                }
            };
            match self.session(&url, &token).await {
                Outcome::Shutdown => return,
                Outcome::Fatal(why) => return self.emit(TetherEvent::Fatal(why)),
                Outcome::Reconnect => {
                    self.emit(TetherEvent::Disconnected);
                    if !self.wait_backoff().await {
                        return;
                    }
                }
                Outcome::AuthFailed(rejected) => {
                    self.resume = None;
                    self.emit(TetherEvent::Disconnected);
                    match self.tokens.refresh_after_rejection(&rejected).await {
                        Ok(_) => {}
                        Err(Error::SignedOut | Error::NotSignedIn) => {
                            return self.emit(TetherEvent::SignedOut);
                        }
                        Err(err) => tracing::warn!(error = %err, "tether token refresh failed"),
                    }
                    if !self.wait_backoff().await {
                        return;
                    }
                }
            }
        }
    }

    /// Sleep for the next backoff step while still taking commands. False on shutdown.
    async fn wait_backoff(&mut self) -> bool {
        self.backoff = if self.backoff.is_zero() {
            self.options.backoff_min
        } else {
            (self.backoff * 2).min(self.options.backoff_max)
        };
        let delay = jitter(self.backoff);
        self.idle(delay).await
    }

    async fn idle(&mut self, delay: Duration) -> bool {
        let until = Instant::now() + delay;
        loop {
            tokio::select! {
                _ = sleep_until(until) => return true,
                cmd = self.commands.recv() => match cmd {
                    None => return false,
                    Some(Command::Shutdown(tx)) => {
                        let _ = tx.send(());
                        return false;
                    }
                    Some(Command::SetGame(g)) => self.game = g,
                },
            }
        }
    }

    async fn session(&mut self, url: &str, token: &Secret) -> Outcome {
        let target = if url.contains('?') {
            url.to_string()
        } else {
            format!("{url}?v=1&encoding=json")
        };
        let ws = match timeout(
            self.options.connect_timeout,
            tokio_tungstenite::connect_async(target),
        )
        .await
        {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(err)) => {
                tracing::info!(error = %err, "tether gateway connect failed");
                return Outcome::Reconnect;
            }
            Err(_) => return Outcome::Reconnect,
        };
        let (mut sink, mut stream) = ws.split();

        // HELLO
        let interval = match timeout(self.options.connect_timeout, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let frame: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                match (
                    frame["op"].as_u64(),
                    frame["d"]["heartbeat_interval"].as_u64(),
                ) {
                    (Some(OP_HELLO), Some(ms)) if ms > 0 => Duration::from_millis(ms),
                    _ => return Outcome::Reconnect,
                }
            }
            _ => return Outcome::Reconnect,
        };

        let hello_reply = match &self.resume {
            Some(r) => json!({ "op": OP_RESUME, "d": {
                "token": token.expose(), "session_id": r.session_id, "seq": self.seq.unwrap_or(0) } }),
            None => json!({ "op": OP_IDENTIFY, "d": {
                "token": token.expose(),
                "properties": { "os": std::env::consts::OS, "browser": "grapple", "device": "grapple" } } }),
        };
        if sink
            .send(Message::text(hello_reply.to_string()))
            .await
            .is_err()
        {
            return Outcome::Reconnect;
        }
        drop(hello_reply);

        let mut ready = false;
        let mut acked = true;
        let mut next_beat = Instant::now() + interval.mul_f64(random_fraction());
        // A connection can stay up for weeks: keep the tokens fresh meanwhile (a refresh token
        // lapses after 90 days unused). `access_token` refreshes only when less than a day is left.
        let mut token_check =
            tokio::time::interval_at(Instant::now() + TOKEN_CHECK_EVERY, TOKEN_CHECK_EVERY);

        macro_rules! send {
            ($v:expr) => {
                if sink.send(Message::text($v.to_string())).await.is_err() {
                    return Outcome::Reconnect;
                }
            };
        }

        loop {
            tokio::select! {
                _ = token_check.tick() => {
                    let tokens = self.tokens.clone();
                    tokio::spawn(async move {
                        let _ = tokens.access_token().await;
                    });
                }
                _ = sleep_until(next_beat) => {
                    if !acked {
                        // A zombie connection: drop it and resume.
                        let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::from(4000), reason: "".into() }))).await;
                        return Outcome::Reconnect;
                    }
                    send!(json!({ "op": OP_HEARTBEAT, "d": self.seq }));
                    acked = false;
                    next_beat = Instant::now() + interval;
                }
                msg = stream.next() => {
                    let text = match msg {
                        None | Some(Err(_)) => return Outcome::Reconnect,
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(Message::Close(frame))) => {
                            return self.closed(frame.map(|f| u16::from(f.code)), token);
                        }
                        Some(Ok(_)) => continue,
                    };
                    let frame: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    match frame["op"].as_u64() {
                        Some(OP_DISPATCH) => {
                            if let Some(s) = frame["s"].as_u64() {
                                self.seq = Some(s);
                            }
                            let t = frame["t"].as_str().unwrap_or("");
                            let d = &frame["d"];
                            match t {
                                "READY" => {
                                    self.on_ready(d);
                                    self.published = None;
                                    ready = true;
                                    self.backoff = Duration::ZERO;
                                    if let Some(frame) = self.game_frame() {
                                        send!(frame);
                                    }
                                }
                                "RESUMED" => {
                                    ready = true;
                                    self.backoff = Duration::ZERO;
                                    self.emit(TetherEvent::Resumed);
                                    // The game may have changed while the connection was down.
                                    if let Some(frame) = self.game_frame() {
                                        send!(frame);
                                    }
                                }
                                "PRESENCE_UPDATE" => self.on_presence(d),
                                "RELATIONSHIP_ADD" => self.on_friend_added(d),
                                "RELATIONSHIP_REMOVE" => self.on_friend_removed(d),
                                "GAME_INVITE_CREATE" | "GAME_INVITE_UPDATE" | "GAME_INVITE_PLAY" => self.on_invite(t, d),
                                _ => {}
                            }
                        }
                        Some(OP_HEARTBEAT) => {
                            send!(json!({ "op": OP_HEARTBEAT, "d": self.seq }));
                        }
                        Some(OP_HEARTBEAT_ACK) => acked = true,
                        Some(OP_RECONNECT) => return Outcome::Reconnect,
                        Some(OP_INVALID_SESSION) => {
                            self.resume = None;
                            self.seq = None;
                            let delay = self.options.invalid_session_delay;
                            let _ = sink.close().await;
                            if !self.idle(delay).await {
                                return Outcome::Shutdown;
                            }
                            // Straight back in (no backoff step): identify fresh.
                            self.backoff = Duration::ZERO;
                            return Outcome::Reconnect;
                        }
                        _ => {}
                    }
                }
                cmd = self.commands.recv() => match cmd {
                    None => {
                        let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                        return Outcome::Shutdown;
                    }
                    Some(Command::Shutdown(tx)) => {
                        let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                        let _ = tx.send(());
                        return Outcome::Shutdown;
                    }
                    Some(Command::SetGame(g)) => {
                        self.game = g;
                        if ready
                            && let Some(frame) = self.game_frame() {
                                send!(frame);
                            }
                    }
                },
            }
        }
    }

    /// Op 3 to bring tether's session in line with the wanted game, if they differ.
    fn game_frame(&mut self) -> Option<Value> {
        if !self.game_allowed || self.game == self.published {
            return None;
        }
        self.published = self.game.clone();
        let activities = match &self.game {
            Some(g) => json!([g.activity()]),
            None => json!([]),
        };
        Some(json!({ "op": OP_PRESENCE_UPDATE, "d": { "activities": activities } }))
    }

    fn closed(&mut self, code: Option<u16>, token: &Secret) -> Outcome {
        match code {
            Some(4004) => Outcome::AuthFailed(token.clone()),
            Some(4100) => Outcome::Fatal("The tether account must confirm its email first.".into()),
            Some(4101) => {
                Outcome::Fatal("tether refused something Grapple is not allowed to do.".into())
            }
            // These end the session: identify again.
            Some(1000 | 1001 | 4002 | 4003 | 4005 | 4007 | 4009) => {
                self.resume = None;
                self.seq = None;
                Outcome::Reconnect
            }
            _ => Outcome::Reconnect,
        }
    }

    fn on_ready(&mut self, d: &Value) {
        let session_id = d["session_id"].as_str().unwrap_or_default().to_string();
        let url = d["resume_gateway_url"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.resume = (!session_id.is_empty()
            && (url.starts_with("ws://") || url.starts_with("wss://")))
        .then_some(Resume { session_id, url });
        self.game_allowed = d["scopes"]
            .as_array()
            .is_none_or(|s| s.iter().any(|v| v.as_str() == Some("presence.game")));
        *self.scopes.write().unwrap() = d["scopes"]
            .as_array()
            .map(|s| s.iter().filter_map(|v| v.as_str().map(String::from)).collect());
        let me: TetherUser = serde_json::from_value(d["user"].clone()).unwrap_or(TetherUser {
            id: String::new(),
            username: String::new(),
            global_name: None,
            avatar: None,
        });
        let mut map = HashMap::new();
        for r in d["relationships"].as_array().into_iter().flatten() {
            if r["type"].as_u64() != Some(1) {
                continue;
            }
            if let Ok(user) = serde_json::from_value::<TetherUser>(r["user"].clone()) {
                map.insert(
                    user.id.clone(),
                    FriendPresence {
                        user,
                        status: Status::Offline,
                        game: None,
                    },
                );
            }
        }
        for p in d["presences"].as_array().into_iter().flatten() {
            if let Some(id) = p["user"]["id"].as_str()
                && let Some(friend) = map.get_mut(id)
            {
                let (status, game) = parse_presence(p);
                friend.status = status;
                friend.game = game;
            }
        }
        *self.friends.write().unwrap() = map.clone();
        let mut friends: Vec<_> = map.into_values().collect();
        friends.sort_by(|a, b| a.user.id.cmp(&b.user.id));
        self.emit(TetherEvent::Ready { me, friends });
    }

    fn on_presence(&mut self, d: &Value) {
        if d.get("guild_id").is_some_and(|g| !g.is_null()) {
            return;
        }
        let Some(id) = d["user"]["id"].as_str() else {
            return;
        };
        let (status, game) = parse_presence(d);
        let updated = {
            let mut friends = self.friends.write().unwrap();
            let friend = friends
                .entry(id.to_string())
                .or_insert_with(|| FriendPresence {
                    user: TetherUser {
                        id: id.to_string(),
                        username: id.to_string(),
                        global_name: None,
                        avatar: None,
                    },
                    status: Status::Offline,
                    game: None,
                });
            friend.status = status;
            friend.game = game;
            friend.clone()
        };
        self.emit(TetherEvent::Friend(updated));
    }

    fn on_friend_added(&mut self, d: &Value) {
        if d["type"].as_u64() != Some(1) {
            return;
        }
        let Ok(user) = serde_json::from_value::<TetherUser>(d["user"].clone()) else {
            return;
        };
        let presence = {
            let mut friends = self.friends.write().unwrap();
            let entry = friends
                .entry(user.id.clone())
                .or_insert_with(|| FriendPresence {
                    user: user.clone(),
                    status: Status::Offline,
                    game: None,
                });
            entry.user = user;
            entry.clone()
        };
        self.emit(TetherEvent::Friend(presence));
    }

    /// `{"invite": APIGameInvite}` (and for PLAY an optional `join`). Unknown fields are ignored; an
    /// invite that does not parse is dropped (never logged: it could hold a name the person typed).
    fn on_invite(&mut self, t: &str, d: &Value) {
        let Ok(invite) = serde_json::from_value::<GameInvite>(d["invite"].clone()) else {
            tracing::info!("tether: an unreadable {t}");
            return;
        };
        self.emit(match t {
            "GAME_INVITE_CREATE" => TetherEvent::InviteCreated(invite),
            "GAME_INVITE_UPDATE" => TetherEvent::InviteUpdated(invite),
            _ => TetherEvent::InvitePlay { invite, join: InviteJoin::from_value(&d["join"]) },
        });
    }

    fn on_friend_removed(&mut self, d: &Value) {
        if d["type"].as_u64() != Some(1) {
            return;
        }
        let Some(id) = d["id"].as_str() else { return };
        self.friends.write().unwrap().remove(id);
        self.emit(TetherEvent::FriendRemoved {
            user_id: id.to_string(),
        });
    }
}

/// Status and game from a presence (`activities`: the one of type 0 is the game).
fn parse_presence(p: &Value) -> (Status, Option<FriendGame>) {
    let status = Status::parse(p["status"].as_str().unwrap_or("offline"));
    if status == Status::Offline {
        return (status, None);
    }
    let game = p["activities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["type"].as_u64() == Some(0))
        .and_then(|a| {
            Some(FriendGame {
                name: a["name"].as_str()?.to_string(),
                since_ms: a["timestamps"]["start"]
                    .as_i64()
                    .or_else(|| a["created_at"].as_i64())?,
                image: a["assets"]["large_image"]
                    .as_str()
                    .and_then(GameImage::parse),
            })
        });
    (status, game)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_friends_game() {
        let p = json!({
            "user": { "id": "1" }, "status": "online",
            "activities": [
                { "name": "Custom Status", "type": 4, "state": "hi" },
                { "name": "Hades II", "type": 0, "created_at": 5, "timestamps": { "start": 4 },
                  "assets": { "large_image": "steam:1145350" } }
            ]
        });
        let (status, game) = parse_presence(&p);
        assert_eq!(status, Status::Online);
        assert_eq!(
            game,
            Some(FriendGame {
                name: "Hades II".into(),
                since_ms: 4,
                image: Some(GameImage::Steam("1145350".into()))
            })
        );
        let offline = json!({ "user": { "id": "1" }, "status": "offline", "activities": [] });
        assert_eq!(parse_presence(&offline), (Status::Offline, None));
    }

    #[test]
    fn wire_game_shape() {
        let g = WireGame {
            name: "X".into(),
            start: 7,
            image: Some("icon:ab".into()),
        };
        assert_eq!(
            g.activity(),
            json!({ "name": "X", "type": 0, "timestamps": { "start": 7 }, "assets": { "large_image": "icon:ab" } })
        );
        let plain = WireGame {
            name: "X".into(),
            start: 7,
            image: None,
        };
        assert!(plain.activity().get("assets").is_none());
    }
}
