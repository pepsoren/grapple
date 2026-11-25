//! A fake tether: the token endpoints, the few REST routes, game invites (section 7) and the
//! gateway of `docs/tether-contract.md`, in memory, on 127.0.0.1. No real tether, no real keyring.
//! Josh is user `100`; his friends are `200` (rowan) and `300` (mira); `400` blocks him.
#![allow(dead_code)]

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State as AxState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::extract::Query;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use grapple_tether::{Browser, Error as TetherError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub enum ServerCmd {
    Send(&'static str, Value),
    Close(u16),
    Drop,
}

pub struct CodeInfo {
    challenge: String,
    redirect_uri: String,
    used: bool,
}

#[derive(Default)]
pub struct FakeState {
    pub codes: HashMap<String, CodeInfo>,
    /// token → still valid
    pub access: HashMap<String, bool>,
    pub refresh: HashMap<String, bool>,
    /// Each token's partner (access ↔ refresh of one sign-in).
    pub pairs: HashMap<String, String>,
    pub expires_in: i64,
    pub token_grants: Vec<String>,
    pub revoked: Vec<String>,
    pub icons: HashSet<String>,
    pub icon_puts: usize,
    pub identifies: Vec<Value>,
    pub resumes: Vec<Value>,
    pub op3: Vec<Value>,
    pub heartbeats: usize,
    /// Stop answering heartbeats (a zombie connection).
    pub no_ack: bool,
    pub heartbeat_interval: u64,
    pub accept_resume: bool,
    pub scopes: Vec<&'static str>,
    pub conn: Option<mpsc::UnboundedSender<ServerCmd>>,
    pub connections: usize,
    pub client_closes: Vec<u16>,
    pub session_ids: HashSet<String>,
    counter: u64,
    /// The `scope` of every authorize request.
    pub asked_scopes: Vec<String>,
    /// false: an older tether without game invites (its invite routes are 404, `invites` is refused).
    pub invites_supported: bool,
    /// Every invite, as tether gives it (oldest first).
    pub invites: Vec<Value>,
    /// `join` payloads by invite id (only ever given to the recipient).
    pub joins: HashMap<String, Value>,
    /// Every body POSTed to `/users/@me/game-invites`.
    pub invite_posts: Vec<Value>,
    /// (path, content-type, body length) of every invite action (`/seen`, `/accept`, ...).
    pub invite_actions: Vec<(String, Option<String>, usize)>,
    /// How many more invites may be sent before 429 (`None`: no limit).
    pub sends_left: Option<usize>,
    pub invites_from: String,
}

pub type Shared = Arc<Mutex<FakeState>>;

pub struct Fake {
    pub addr: SocketAddr,
    pub state: Shared,
}

fn token(prefix: &str) -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).unwrap();
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(b))
}

impl Fake {
    pub async fn start() -> Fake {
        let state: Shared = Arc::new(Mutex::new(FakeState {
            expires_in: 604_800,
            heartbeat_interval: 41_250,
            accept_resume: true,
            scopes: vec!["identify", "presence.game", "friends.read", "invites"],
            invites_supported: true,
            invites_from: "friends".into(),
            ..Default::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/api/v1/gateway", get(gateway_info))
            // Like the real tether with app sign-in: the authorize route exists and wants a session.
            .route("/api/v1/oauth2/authorize", get(|| async { StatusCode::UNAUTHORIZED }))
            .route("/test/authorize", post(test_authorize))
            .route("/api/v1/oauth2/token", post(token_endpoint))
            .route("/api/v1/oauth2/token/revoke", post(revoke_endpoint))
            .route("/api/v1/users/@me", get(me))
            .route("/api/v1/game-icons/{sha}", get(head_icon).put(put_icon))
            .route("/api/v1/users/@me/game-invites", get(list_invites).post(send_invite))
            .route("/api/v1/users/@me/game-invites/{id}", get(get_invite))
            .route("/api/v1/users/@me/game-invites/{id}/{action}", post(invite_action))
            .route("/api/v1/users/@me/game-invite-settings", get(get_invite_settings).put(put_invite_settings))
            .route("/gateway", get(gateway))
            .with_state((state.clone(), addr));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Fake { addr, state }
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut FakeState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    pub fn cmd(&self, cmd: ServerCmd) {
        self.with(|s| {
            s.conn
                .as_ref()
                .expect("a gateway connection")
                .send(cmd)
                .ok()
        });
    }

    /// Wait until `pred` holds (or panic after 5 s).
    pub async fn until(&self, what: &str, pred: impl Fn(&FakeState) -> bool) {
        for _ in 0..500 {
            if pred(&self.state.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    /// An invite in the fake's store (e.g. one a friend sent Josh), with an optional `join` payload.
    pub fn seed_invite(&self, id: &str, sender: &str, recipient: &str, state: &str, expires_at: &str, join: Option<Value>) {
        self.with(|s| {
            s.invites.push(invite_json(id, sender, recipient, json!({ "id": "steam:252950", "name": "Rocket League", "image": "steam:252950" }), state, expires_at, join.is_some()));
            if let Some(j) = join {
                s.joins.insert(id.to_string(), j);
            }
        });
    }

    /// The stored invite with this id.
    pub fn invite(&self, id: &str) -> Value {
        self.with(|s| s.invites.iter().find(|i| i["id"] == id).cloned().expect("an invite"))
    }

    /// Make every token so far stop working (as if revoked in tether's Settings).
    pub fn revoke_all(&self) {
        self.with(|s| {
            s.access.values_mut().for_each(|v| *v = false);
            s.refresh.values_mut().for_each(|v| *v = false);
        });
    }
}

type St = (Shared, SocketAddr);

async fn gateway_info(AxState((_, addr)): AxState<St>) -> Json<Value> {
    Json(json!({ "url": format!("ws://{addr}/gateway") }))
}

/// Stands in for the web page's Allow / Cancel: validates like tether and answers `{location}`.
async fn test_authorize(
    AxState((state, _)): AxState<St>,
    Json(q): Json<HashMap<String, String>>,
) -> Response {
    let scope = q.get("scope").cloned().unwrap_or_default();
    let supported = {
        let mut s = state.lock().unwrap();
        s.asked_scopes.push(scope.clone());
        s.invites_supported
    };
    let ok = q.get("client_id").map(String::as_str) == Some("grapple")
        && q.get("response_type").map(String::as_str) == Some("code")
        && q.get("code_challenge_method").map(String::as_str) == Some("S256")
        && (scope == "identify presence.game friends.read" || (supported && scope == "identify presence.game friends.read invites"))
        && q["redirect_uri"].starts_with("http://127.0.0.1:")
        && q["redirect_uri"].ends_with("/callback")
        && q.get("code_challenge").is_some_and(|c| c.len() == 43);
    if !ok {
        return (StatusCode::BAD_REQUEST, "bad authorize request").into_response();
    }
    let redirect = q["redirect_uri"].clone();
    if q.contains_key("deny") {
        return Json(
            json!({ "location": format!("{redirect}?error=access_denied&state={}", q["state"]) }),
        )
        .into_response();
    }
    let code = token("");
    state.lock().unwrap().codes.insert(
        code.clone(),
        CodeInfo {
            challenge: q["code_challenge"].clone(),
            redirect_uri: redirect.clone(),
            used: false,
        },
    );
    let state_param = q.get("override_state").unwrap_or(&q["state"]).clone();
    Json(json!({ "location": format!("{redirect}?code={code}&state={state_param}") }))
        .into_response()
}

fn oauth_error(error: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error, "error_description": "fake" })),
    )
        .into_response()
}

fn issue(s: &mut FakeState) -> Response {
    let access = token("tapp.");
    let refresh = token("tapr.");
    s.access.insert(access.clone(), true);
    s.refresh.insert(refresh.clone(), true);
    s.pairs.insert(access.clone(), refresh.clone());
    s.pairs.insert(refresh.clone(), access.clone());
    Json(json!({
        "access_token": access, "token_type": "Bearer", "expires_in": s.expires_in,
        "refresh_token": refresh, "scope": s.scopes.join(" "),
    }))
    .into_response()
}

async fn token_endpoint(
    AxState((state, _)): AxState<St>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let mut s = state.lock().unwrap();
    if f.get("client_id").map(String::as_str) != Some("grapple") {
        return oauth_error("invalid_client");
    }
    match f.get("grant_type").map(String::as_str) {
        Some("authorization_code") => {
            s.token_grants.push("authorization_code".into());
            let Some(info) = f.get("code").and_then(|c| s.codes.get_mut(c)) else {
                return oauth_error("invalid_grant");
            };
            if info.used {
                return oauth_error("invalid_grant");
            }
            info.used = true;
            let verifier = f.get("code_verifier").cloned().unwrap_or_default();
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            if challenge != info.challenge || f.get("redirect_uri") != Some(&info.redirect_uri) {
                return oauth_error("invalid_grant");
            }
            issue(&mut s)
        }
        Some("refresh_token") => {
            s.token_grants.push("refresh_token".into());
            let rt = f.get("refresh_token").cloned().unwrap_or_default();
            if s.refresh.get(&rt) != Some(&true) {
                return oauth_error("invalid_grant");
            }
            s.refresh.insert(rt.clone(), false);
            // Rotation: the old access token stops too.
            if let Some(a) = s.pairs.get(&rt).cloned() {
                s.access.insert(a, false);
            }
            issue(&mut s)
        }
        _ => oauth_error("unsupported_grant_type"),
    }
}

async fn revoke_endpoint(
    AxState((state, _)): AxState<St>,
    Form(f): Form<HashMap<String, String>>,
) -> StatusCode {
    let mut s = state.lock().unwrap();
    if let Some(t) = f.get("token") {
        s.revoked.push(t.clone());
        // Only this sign-in (the token and its partner).
        let partner = s.pairs.get(t).cloned();
        for tok in [Some(t.clone()), partner].into_iter().flatten() {
            if let Some(v) = s.access.get_mut(&tok) {
                *v = false;
            }
            if let Some(v) = s.refresh.get_mut(&tok) {
                *v = false;
            }
        }
    }
    StatusCode::OK
}

fn bearer_ok(state: &Shared, headers: &HeaderMap) -> bool {
    let h = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    h.strip_prefix("Bearer ")
        .is_some_and(|t| state.lock().unwrap().access.get(t) == Some(&true))
}

async fn me(AxState((state, _)): AxState<St>, headers: HeaderMap) -> Response {
    if !bearer_ok(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({ "id": "100", "username": "josh", "global_name": "Josh", "avatar": null, "discriminator": "0" }))
        .into_response()
}

async fn head_icon(
    AxState((state, _)): AxState<St>,
    headers: HeaderMap,
    Path(sha): Path<String>,
) -> StatusCode {
    if !bearer_ok(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    if state.lock().unwrap().icons.contains(&sha) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn put_icon(
    AxState((state, _)): AxState<St>,
    headers: HeaderMap,
    Path(sha): Path<String>,
    body: axum::body::Bytes,
) -> StatusCode {
    if !bearer_ok(&state, &headers) {
        return StatusCode::UNAUTHORIZED;
    }
    if headers.get("content-type").and_then(|v| v.to_str().ok()) != Some("image/png") {
        return StatusCode::BAD_REQUEST;
    }
    let real: String = Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if real != sha || body.len() > 64 * 1024 {
        return StatusCode::BAD_REQUEST;
    }
    let mut s = state.lock().unwrap();
    s.icons.insert(sha);
    s.icon_puts += 1;
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------------------------------------- invites

fn api_error(status: StatusCode, code: u32, message: &str) -> Response {
    (status, Json(json!({ "code": code, "message": message }))).into_response()
}

const ME: &str = "100";
const FRIENDS: [&str; 2] = ["200", "300"];

fn invite_json(id: &str, sender: &str, recipient: &str, game: Value, state: &str, expires_at: &str, joinable: bool) -> Value {
    json!({
        "id": id, "channel_id": "9000", "message_id": id, "sender_id": sender, "recipient_id": recipient,
        "game": game, "state": state, "joinable": joinable,
        "created_at": "2026-11-25T18:00:00.000Z", "expires_at": expires_at,
        "seen_at": null, "answered_at": null,
        // A field a newer tether might add: Grapple must ignore it.
        "card_style": "compact"
    })
}

/// The invite routes' gate: a valid app token, a tether that has invites, and the `invites` scope.
fn invite_gate(state: &Shared, headers: &HeaderMap) -> Result<(), Response> {
    if !state.lock().unwrap().invites_supported {
        return Err(api_error(StatusCode::NOT_FOUND, 0, "404: Not Found"));
    }
    if !bearer_ok(state, headers) {
        return Err(api_error(StatusCode::UNAUTHORIZED, 40001, "401: Unauthorized"));
    }
    if !state.lock().unwrap().scopes.contains(&"invites") {
        return Err(api_error(StatusCode::FORBIDDEN, 50001, "Missing Access"));
    }
    Ok(())
}

fn dispatch_update(s: &FakeState, t: &'static str, invite: &Value) {
    if let Some(conn) = &s.conn {
        let _ = conn.send(ServerCmd::Send(t, json!({ "invite": invite })));
    }
}

async fn send_invite(AxState((state, _)): AxState<St>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    let mut s = state.lock().unwrap();
    s.invite_posts.push(body.clone());
    let fields_ok = body.as_object().is_some_and(|o| o.keys().all(|k| ["recipient_id", "game", "expires_in", "join"].contains(&k.as_str())))
        && body["game"].as_object().is_some_and(|o| o.keys().all(|k| ["id", "name", "image"].contains(&k.as_str())))
        && body["expires_in"].as_u64().is_none_or(|e| (60..=3600).contains(&e));
    if !fields_ok {
        return api_error(StatusCode::BAD_REQUEST, 50035, "Invalid Form Body");
    }
    if let Some(left) = s.sends_left.as_mut() {
        if *left == 0 {
            return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "30")], Json(json!({ "code": 90003, "message": "You are being rate limited.", "retry_after": 29.5, "global": false }))).into_response();
        }
        *left -= 1;
    }
    let to = body["recipient_id"].as_str().unwrap_or("");
    if to == "400" {
        return api_error(StatusCode::FORBIDDEN, 50007, "Cannot send messages to this user");
    }
    if !FRIENDS.contains(&to) {
        return api_error(StatusCode::NOT_FOUND, 10013, "Unknown User");
    }
    s.counter += 1;
    let id = format!("15751623975119{:05}", s.counter);
    let game = json!({ "id": body["game"]["id"], "name": body["game"]["name"], "image": body["game"]["image"].as_str().map(String::from).or_else(|| body["game"]["id"].as_str().filter(|g| g.starts_with("steam:")).map(String::from)) });
    let invite = invite_json(&id, ME, to, game, "sent", "2099-01-01T00:00:00.000Z", false);
    s.invites.push(invite.clone());
    dispatch_update(&s, "GAME_INVITE_CREATE", &invite);
    (StatusCode::CREATED, Json(invite)).into_response()
}

async fn list_invites(AxState((state, _)): AxState<St>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
    let s = state.lock().unwrap();
    Json(Value::Array(s.invites.iter().rev().take(limit).cloned().collect())).into_response()
}

async fn get_invite(AxState((state, _)): AxState<St>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    let s = state.lock().unwrap();
    let Some(mut invite) = s.invites.iter().find(|i| i["id"] == id.as_str() && (i["sender_id"] == ME || i["recipient_id"] == ME)).cloned() else {
        return api_error(StatusCode::NOT_FOUND, 90270, "Unknown Game Invite");
    };
    if invite["recipient_id"] == ME
        && ["sent", "seen", "accepted"].contains(&invite["state"].as_str().unwrap_or(""))
        && let Some(j) = s.joins.get(&id)
    {
        invite["join"] = j.clone();
    }
    Json(invite).into_response()
}

async fn invite_action(AxState((state, _)): AxState<St>, headers: HeaderMap, Path((id, action)): Path<(String, String)>, body: axum::body::Bytes) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    let mut s = state.lock().unwrap();
    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).map(String::from);
    s.invite_actions.push((format!("{id}/{action}"), ct, body.len()));
    let Some(i) = s.invites.iter().position(|i| i["id"] == id.as_str() && (i["sender_id"] == ME || i["recipient_id"] == ME)) else {
        return api_error(StatusCode::NOT_FOUND, 90270, "Unknown Game Invite");
    };
    let invite = &s.invites[i];
    let mine = if action == "cancel" { invite["sender_id"] == ME } else { invite["recipient_id"] == ME };
    if !mine {
        return api_error(StatusCode::FORBIDDEN, 50001, "Missing Access");
    }
    let st = invite["state"].as_str().unwrap_or("").to_string();
    let past = invite["expires_at"].as_str().is_some_and(|e| e < "2026");
    let open = (st == "sent" || st == "seen") && !past;
    let next = match action.as_str() {
        "seen" => if st == "sent" && !past { "seen" } else { st.as_str() },
        "accept" if open || (st == "accepted" && !past) => "accepted",
        "decline" if open || st == "declined" => "declined",
        "cancel" if open || st == "cancelled" => "cancelled",
        "accept" | "decline" | "cancel" => return api_error(StatusCode::BAD_REQUEST, 90271, "This game invite is no longer open."),
        _ => return api_error(StatusCode::NOT_FOUND, 0, "404: Not Found"),
    }
    .to_string();
    let changed = next != st;
    s.invites[i]["state"] = json!(next);
    if changed && action != "seen" {
        s.invites[i]["answered_at"] = json!("2026-11-25T18:05:00.000Z");
    }
    let invite = s.invites[i].clone();
    if changed {
        dispatch_update(&s, "GAME_INVITE_UPDATE", &invite);
    }
    if action == "accept" {
        return Json(json!({ "invite": invite, "app_notified": false })).into_response();
    }
    Json(invite).into_response()
}

async fn get_invite_settings(AxState((state, _)): AxState<St>, headers: HeaderMap) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    Json(json!({ "from": state.lock().unwrap().invites_from })).into_response()
}

async fn put_invite_settings(AxState((state, _)): AxState<St>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if let Err(r) = invite_gate(&state, &headers) {
        return r;
    }
    match body["from"].as_str() {
        Some(f @ ("friends" | "nobody")) if body.as_object().is_some_and(|o| o.len() == 1) => {
            state.lock().unwrap().invites_from = f.to_string();
            Json(json!({ "from": f })).into_response()
        }
        _ => api_error(StatusCode::BAD_REQUEST, 50035, "Invalid Form Body"),
    }
}

async fn gateway(ws: WebSocketUpgrade, AxState((state, addr)): AxState<St>) -> Response {
    ws.on_upgrade(move |socket| run_socket(socket, state, addr))
}

fn frame(op: u64, d: Value) -> Message {
    Message::Text(
        json!({ "op": op, "d": d, "s": null, "t": null })
            .to_string()
            .into(),
    )
}

async fn run_socket(mut socket: WebSocket, state: Shared, addr: SocketAddr) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let interval = {
        let mut s = state.lock().unwrap();
        s.conn = Some(tx);
        s.connections += 1;
        s.heartbeat_interval
    };
    let mut seq = 0u64;
    let mut session: Option<String> = None;
    if socket
        .send(frame(10, json!({ "heartbeat_interval": interval })))
        .await
        .is_err()
    {
        return;
    }
    macro_rules! dispatch {
        ($t:expr, $d:expr) => {{
            seq += 1;
            let text = json!({ "op": 0, "s": seq, "t": $t, "d": $d }).to_string();
            if socket.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }};
    }
    loop {
        tokio::select! {
            msg = socket.recv() => {
                let text = match msg {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Close(f))) => {
                        state.lock().unwrap().client_closes.push(f.map(|f| f.code).unwrap_or(1005));
                        return;
                    }
                    Some(Ok(_)) => continue,
                    _ => return,
                };
                let v: Value = serde_json::from_str(&text).unwrap();
                match v["op"].as_u64().unwrap() {
                    1 => {
                        let no_ack = {
                            let mut s = state.lock().unwrap();
                            s.heartbeats += 1;
                            s.no_ack
                        };
                        if !no_ack {
                            let _ = socket.send(frame(11, Value::Null)).await;
                        }
                    }
                    2 => {
                        let tok = v["d"]["token"].as_str().unwrap_or("").to_string();
                        let (valid, scopes) = {
                            let mut s = state.lock().unwrap();
                            s.identifies.push(v["d"].clone());
                            (s.access.get(&tok) == Some(&true), s.scopes.clone())
                        };
                        if !valid {
                            let _ = socket.send(Message::Close(Some(CloseFrame { code: 4004, reason: "Authentication failed".into() }))).await;
                            return;
                        }
                        let sid = {
                            let mut s = state.lock().unwrap();
                            s.counter += 1;
                            let sid = format!("session{}", s.counter);
                            s.session_ids.insert(sid.clone());
                            sid
                        };
                        session = Some(sid.clone());
                        dispatch!("READY", json!({
                            "v": 1,
                            "user": { "id": "100", "username": "josh", "global_name": "Josh", "avatar": null },
                            "session_id": sid,
                            "resume_gateway_url": format!("ws://{addr}/gateway"),
                            "application": { "id": "grapple", "name": "Grapple", "description": "" },
                            "scopes": scopes,
                            "guilds": [],
                            "relationships": [
                                { "id": "200", "type": 1, "nickname": null, "since": "2026-01-01T00:00:00Z",
                                  "user": { "id": "200", "username": "rowan", "global_name": "Rowan", "avatar": "abc" } },
                                { "id": "300", "type": 1, "nickname": null, "since": "2026-01-01T00:00:00Z",
                                  "user": { "id": "300", "username": "mira", "global_name": null, "avatar": null } }
                            ],
                            "presences": [
                                { "user": { "id": "200" }, "status": "online", "activities": [
                                    { "name": "Hades II", "type": 0, "created_at": 1_760_000_100_000i64,
                                      "timestamps": { "start": 1_760_000_000_000i64 },
                                      "assets": { "large_image": "steam:1145350" } } ] }
                            ]
                        }));
                    }
                    3 => {
                        state.lock().unwrap().op3.push(v["d"].clone());
                    }
                    6 => {
                        let tok = v["d"]["token"].as_str().unwrap_or("").to_string();
                        let sid = v["d"]["session_id"].as_str().unwrap_or("").to_string();
                        let ok = {
                            let mut s = state.lock().unwrap();
                            s.resumes.push(v["d"].clone());
                            s.accept_resume && s.access.get(&tok) == Some(&true) && s.session_ids.contains(&sid)
                        };
                        if ok {
                            seq = v["d"]["seq"].as_u64().unwrap_or(0);
                            session = Some(sid);
                            dispatch!("RESUMED", Value::Null);
                        } else {
                            let _ = socket.send(frame(9, json!(false))).await;
                        }
                    }
                    _ => {}
                }
            }
            cmd = rx.recv() => match cmd {
                Some(ServerCmd::Send(t, d)) => dispatch!(t, d),
                Some(ServerCmd::Close(code)) => {
                    let _ = socket.send(Message::Close(Some(CloseFrame { code, reason: "".into() }))).await;
                    return;
                }
                Some(ServerCmd::Drop) | None => {
                    let _ = session;
                    return;
                }
            }
        }
    }
}

/// Plays the person in the browser: hands the authorize request to the fake (Allow or Cancel),
/// then follows the answer to Grapple's loopback listener. Never opens a real browser.
pub struct FakeBrowser {
    pub origin: String,
    pub deny: bool,
    /// First come back with a wrong `state` (the listener must ignore it), then the right one.
    pub wrong_state_first: bool,
    /// Do nothing at all (the listener should time out).
    pub silent: bool,
    pub opened: Arc<Mutex<Vec<String>>>,
}

impl FakeBrowser {
    pub fn new(origin: String) -> Self {
        FakeBrowser {
            origin,
            deny: false,
            wrong_state_first: false,
            silent: false,
            opened: Arc::default(),
        }
    }
}

impl Browser for FakeBrowser {
    fn open(&self, url: &url::Url) -> Result<(), TetherError> {
        self.opened.lock().unwrap().push(url.path().to_string());
        assert!(
            url.as_str()
                .starts_with(&format!("{}/oauth2/authorize?", self.origin))
        );
        if self.silent {
            return Ok(());
        }
        let mut q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if self.deny {
            q.insert("deny".into(), "1".into());
        }
        let origin = self.origin.clone();
        let wrong_first = self.wrong_state_first;
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            if wrong_first {
                let mut bad = q.clone();
                bad.insert("override_state".into(), "not-the-state-grapple-made".into());
                let loc: Value = http
                    .post(format!("{origin}/test/authorize"))
                    .json(&bad)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                let res = http
                    .get(loc["location"].as_str().unwrap())
                    .send()
                    .await
                    .unwrap();
                assert_eq!(res.status(), 400);
            }
            let loc: Value = http
                .post(format!("{origin}/test/authorize"))
                .json(&q)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let res = http
                .get(loc["location"].as_str().unwrap())
                .send()
                .await
                .unwrap();
            assert!(res.status().is_success());
            assert!(
                res.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html")
            );
        });
        Ok(())
    }
}
