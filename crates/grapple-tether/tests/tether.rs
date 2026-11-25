//! grapple-tether against the fake tether in `common/` (no real tether, no real keyring, no real
//! browser): sign-in, tokens, refresh, the gateway, the game, icons, resume, revocation, sign-out,
//! and game invites.

mod common;

use common::{Fake, FakeBrowser, ServerCmd};
use grapple_tether::{
    Error, GameActivity, GameImage, GatewayHandle, GatewayOptions, InviteGame, InviteJoin, InviteState, InvitesFrom,
    MemoryStore, Status, Tether, TetherConfig, TetherEvent,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

fn options() -> GatewayOptions {
    GatewayOptions {
        backoff_min: Duration::from_millis(20),
        backoff_max: Duration::from_millis(100),
        invalid_session_delay: Duration::from_millis(20),
        connect_timeout: Duration::from_secs(3),
    }
}

async fn setup() -> (Fake, Tether, Arc<MemoryStore>) {
    let fake = Fake::start().await;
    let store = Arc::new(MemoryStore::new());
    let tether = Tether::new(TetherConfig::new(&fake.origin()).unwrap(), store.clone()).unwrap();
    (fake, tether, store)
}

async fn signed_in() -> (Fake, Tether, Arc<MemoryStore>) {
    let (fake, tether, store) = setup().await;
    tether
        .sign_in(&FakeBrowser::new(fake.origin()))
        .await
        .unwrap();
    (fake, tether, store)
}

async fn next_event(
    rx: &mut broadcast::Receiver<TetherEvent>,
    pred: impl Fn(&TetherEvent) -> bool,
) -> TetherEvent {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = rx.recv().await.expect("event stream open");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("timed out waiting for an event")
}

async fn connected(
    tether: &Tether,
) -> (GatewayHandle, broadcast::Receiver<TetherEvent>, TetherEvent) {
    let gw = tether.connect_with(options());
    let mut rx = gw.subscribe();
    let ready = next_event(&mut rx, |e| matches!(e, TetherEvent::Ready { .. })).await;
    (gw, rx, ready)
}

fn png(seed: u8) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend(std::iter::repeat_n(seed, 100));
    v
}

#[tokio::test]
async fn signs_in_through_the_browser_and_keeps_the_tokens() {
    let (fake, tether, store) = setup().await;
    assert!(!tether.is_signed_in().await.unwrap());
    let browser = FakeBrowser::new(fake.origin());
    let me = tether.sign_in(&browser).await.unwrap();
    assert_eq!(me.username, "josh");
    assert_eq!(me.display_name(), "Josh");
    assert!(tether.is_signed_in().await.unwrap());
    let stored = store.peek().unwrap();
    assert!(stored.access_token.expose().starts_with("tapp."));
    assert!(stored.refresh_token.expose().starts_with("tapr."));
    assert_eq!(stored.scopes, ["identify", "presence.game", "friends.read", "invites"]);
    assert_eq!(fake.with(|s| s.asked_scopes.clone()), ["identify presence.game friends.read invites"]);
    // The fake checked PKCE (S256 of the verifier) and the exact redirect_uri.
    assert_eq!(
        fake.with(|s| s.token_grants.clone()),
        ["authorization_code"]
    );
    assert_eq!(*browser.opened.lock().unwrap(), ["/oauth2/authorize"]);
}

#[tokio::test]
async fn debug_output_never_shows_a_token() {
    let (_fake, _tether, store) = signed_in().await;
    let tokens = store.peek().unwrap();
    let printed = format!("{tokens:?}");
    assert!(!printed.contains(tokens.access_token.expose()));
    assert!(!printed.contains(tokens.refresh_token.expose()));
    assert!(printed.contains("Secret(***)"));
}

#[tokio::test]
async fn cancel_in_the_browser_is_denied_and_stores_nothing() {
    let (fake, tether, store) = setup().await;
    let mut browser = FakeBrowser::new(fake.origin());
    browser.deny = true;
    assert!(matches!(tether.sign_in(&browser).await, Err(Error::Denied)));
    assert!(store.peek().is_none());
}

#[tokio::test]
async fn an_answer_with_the_wrong_state_is_ignored() {
    let (fake, tether, _store) = setup().await;
    let mut browser = FakeBrowser::new(fake.origin());
    browser.wrong_state_first = true;
    let me = tether.sign_in(&browser).await.unwrap();
    assert_eq!(me.id, "100");
    // Only the right one was traded.
    assert_eq!(fake.with(|s| s.token_grants.len()), 1);
}

#[tokio::test]
async fn sign_in_gives_up_when_nobody_answers() {
    let (fake, mut tether, store) = setup().await;
    tether.set_sign_in_timeout(Duration::from_millis(200));
    let mut browser = FakeBrowser::new(fake.origin());
    browser.silent = true;
    assert!(matches!(
        tether.sign_in(&browser).await,
        Err(Error::Timeout)
    ));
    assert!(store.peek().is_none());
}

/// An older tether (no app sign-in: its authorize route is a 404) is caught before the browser
/// opens, and a tether that does not answer at all says so; nothing is stored either way.
#[tokio::test]
async fn sign_in_checks_the_instance_before_opening_the_browser() {
    // A server that only knows 404.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut s, _)) = l.accept().await {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let _ = s
                .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });
    let store = Arc::new(MemoryStore::new());
    let tether = Tether::new(TetherConfig::new(&old).unwrap(), store.clone()).unwrap();
    let browser = FakeBrowser::new(old.clone());
    assert!(matches!(tether.sign_in(&browser).await, Err(Error::AppsUnsupported)));
    assert!(browser.opened.lock().unwrap().is_empty(), "the browser must not open");
    assert!(store.peek().is_none());

    let gone = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        format!("http://{a}")
    };
    let tether = Tether::new(TetherConfig::new(&gone).unwrap(), store.clone()).unwrap();
    let browser = FakeBrowser::new(gone.clone());
    match tether.sign_in(&browser).await {
        Err(Error::Unreachable(host)) => assert!(gone.ends_with(&host)),
        other => panic!("expected Unreachable, got {other:?}"),
    }
    assert!(browser.opened.lock().unwrap().is_empty());
}

#[tokio::test]
async fn refreshes_before_the_access_token_runs_out() {
    let (fake, tether, store) = setup().await;
    fake.with(|s| s.expires_in = 60); // less than the one-day margin
    tether
        .sign_in(&FakeBrowser::new(fake.origin()))
        .await
        .unwrap();
    let before = store.peek().unwrap();
    // sign_in's own GET /users/@me already refreshed (the token was close to running out).
    assert_eq!(
        fake.with(|s| s.token_grants.clone()),
        ["authorization_code", "refresh_token"]
    );
    fake.with(|s| s.expires_in = 604_800);
    tether.api().me().await.unwrap();
    let after = store.peek().unwrap();
    assert_ne!(before.refresh_token, after.refresh_token);
    assert_eq!(fake.with(|s| s.token_grants.len()), 3);
    tether.api().me().await.unwrap();
    assert_eq!(
        fake.with(|s| s.token_grants.len()),
        3,
        "a fresh token is not refreshed again"
    );
}

#[tokio::test]
async fn a_refused_refresh_signs_out_and_forgets_the_tokens() {
    let (fake, tether, store) = signed_in().await;
    fake.revoke_all();
    assert!(matches!(tether.api().me().await, Err(Error::SignedOut)));
    assert!(store.peek().is_none());
    assert!(!tether.is_signed_in().await.unwrap());
}

#[tokio::test]
async fn a_401_is_refreshed_once_and_retried() {
    let (fake, tether, store) = signed_in().await;
    // The access token stops (e.g. it ran out early) but the refresh token still works.
    fake.with(|s| s.access.values_mut().for_each(|v| *v = false));
    let me = tether.api().me().await.unwrap();
    assert_eq!(me.id, "100");
    assert!(store.peek().is_some());
    assert_eq!(
        fake.with(|s| s.token_grants.last().cloned()),
        Some("refresh_token".into())
    );
}

#[tokio::test]
async fn ready_lists_friends_and_presence_updates_stream_in() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, ready) = connected(&tether).await;
    let TetherEvent::Ready { me, friends } = ready else {
        unreachable!()
    };
    assert_eq!(me.username, "josh");
    assert_eq!(friends.len(), 2);
    let rowan = friends.iter().find(|f| f.user.id == "200").unwrap();
    assert_eq!(rowan.status, Status::Online);
    let game = rowan.game.as_ref().unwrap();
    assert_eq!(game.name, "Hades II");
    assert_eq!(game.since_ms, 1_760_000_000_000);
    assert_eq!(game.image, Some(GameImage::Steam("1145350".into())));
    let mira = friends.iter().find(|f| f.user.id == "300").unwrap();
    assert_eq!((mira.status, mira.game.is_none()), (Status::Offline, true));
    // IDENTIFY carried the app token and Grapple's properties.
    let identify = fake.with(|s| s.identifies[0].clone());
    assert!(identify["token"].as_str().unwrap().starts_with("tapp."));
    assert_eq!(identify["properties"]["browser"], "grapple");

    fake.cmd(ServerCmd::Send(
        "PRESENCE_UPDATE",
        json!({ "user": { "id": "300" }, "status": "idle", "activities": [
            { "name": "Celeste", "type": 0, "created_at": 2, "timestamps": { "start": 1 },
              "assets": { "large_image": format!("icon:{}", "a".repeat(64)) } } ] }),
    ));
    let TetherEvent::Friend(p) = next_event(&mut rx, |e| matches!(e, TetherEvent::Friend(_))).await
    else {
        unreachable!()
    };
    assert_eq!(p.user.username, "mira");
    assert_eq!(p.status, Status::Idle);
    assert_eq!(p.game.unwrap().image, Some(GameImage::Icon("a".repeat(64))));

    fake.cmd(ServerCmd::Send(
        "RELATIONSHIP_REMOVE",
        json!({ "id": "200", "type": 1, "nickname": null }),
    ));
    next_event(
        &mut rx,
        |e| matches!(e, TetherEvent::FriendRemoved { user_id } if user_id == "200"),
    )
    .await;
    assert_eq!(
        gw.friends()
            .iter()
            .map(|f| f.user.id.as_str())
            .collect::<Vec<_>>(),
        ["300"]
    );

    // Friend requests (type 3) are not friends.
    fake.cmd(ServerCmd::Send(
        "RELATIONSHIP_ADD",
        json!({ "id": "400", "type": 3, "nickname": null,
        "since": "x", "user": { "id": "400", "username": "stranger" } }),
    ));
    fake.cmd(ServerCmd::Send(
        "RELATIONSHIP_ADD",
        json!({ "id": "500", "type": 1, "nickname": null,
        "since": "x", "user": { "id": "500", "username": "kai" } }),
    ));
    let TetherEvent::Friend(kai) =
        next_event(&mut rx, |e| matches!(e, TetherEvent::Friend(_))).await
    else {
        unreachable!()
    };
    assert_eq!(kai.user.username, "kai");
    assert!(gw.friends().iter().all(|f| f.user.id != "400"));
    gw.shutdown().await;
}

#[tokio::test]
async fn set_game_sends_op_3_once_and_clears_it() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, _rx, _) = connected(&tether).await;
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        - 60_000;
    gw.set_game(Some(GameActivity::steam(
        "Deep Rock Galactic",
        548430,
        started,
    )))
    .await
    .unwrap();
    fake.until("op 3", |s| s.op3.len() == 1).await;
    assert_eq!(
        fake.with(|s| s.op3[0].clone()),
        json!({ "activities": [{ "name": "Deep Rock Galactic", "type": 0,
            "timestamps": { "start": started }, "assets": { "large_image": "steam:548430" } }] })
    );
    // The same game again sends nothing.
    gw.set_game(Some(GameActivity::steam(
        "Deep Rock Galactic",
        548430,
        started,
    )))
    .await
    .unwrap();
    gw.set_game(None).await.unwrap();
    fake.until("clear", |s| s.op3.len() == 2).await;
    assert_eq!(fake.with(|s| s.op3[1].clone()), json!({ "activities": [] }));
    // A start in the future is pulled back to now.
    gw.set_game(Some(GameActivity::plain("Later", started + 3_600_000)))
        .await
        .unwrap();
    fake.until("future start", |s| s.op3.len() == 3).await;
    let sent = fake.with(|s| {
        s.op3[2]["activities"][0]["timestamps"]["start"]
            .as_i64()
            .unwrap()
    });
    assert!(sent <= started + 120_000);
    gw.shutdown().await;
    fake.until("clean close", |s| s.client_closes.contains(&1000))
        .await;
}

#[tokio::test]
async fn a_non_steam_icon_is_uploaded_once_and_used_as_icon_sha() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, _rx, _) = connected(&tether).await;
    let icon = png(7);
    gw.set_game(Some(GameActivity::with_icon(
        "Fortnite",
        icon.clone(),
        1_760_000_000_000,
    )))
    .await
    .unwrap();
    fake.until("op 3", |s| s.op3.len() == 1).await;
    let image = fake.with(|s| {
        s.op3[0]["activities"][0]["assets"]["large_image"]
            .as_str()
            .unwrap()
            .to_string()
    });
    let sha = image.strip_prefix("icon:").unwrap().to_string();
    assert!(fake.with(|s| s.icons.contains(&sha)));
    assert_eq!(fake.with(|s| s.icon_puts), 1);
    gw.set_game(None).await.unwrap();
    gw.set_game(Some(GameActivity::with_icon(
        "Fortnite",
        icon,
        1_760_000_000_000,
    )))
    .await
    .unwrap();
    fake.until("op 3 again", |s| s.op3.len() == 3).await;
    assert_eq!(
        fake.with(|s| s.icon_puts),
        1,
        "HEAD found it: no second upload"
    );
    // A file too big for tether goes out without a picture.
    gw.set_game(Some(GameActivity::with_icon(
        "Huge",
        vec![0x89; 70 * 1024],
        1_760_000_000_000,
    )))
    .await
    .unwrap();
    fake.until("op 3 without picture", |s| s.op3.len() == 4)
        .await;
    assert!(fake.with(|s| s.op3[3]["activities"][0].get("assets").is_none()));
    gw.shutdown().await;
}

#[tokio::test]
async fn heartbeats_and_acks() {
    let (fake, tether, _store) = signed_in().await;
    fake.with(|s| s.heartbeat_interval = 80);
    let (gw, _rx, _) = connected(&tether).await;
    fake.until("heartbeats", |s| s.heartbeats >= 3).await;
    assert_eq!(
        fake.with(|s| s.connections),
        1,
        "acked heartbeats keep the connection"
    );
    gw.shutdown().await;
}

#[tokio::test]
async fn a_connection_that_stops_acking_heartbeats_is_dropped_and_resumed() {
    let (fake, tether, _store) = signed_in().await;
    fake.with(|s| {
        s.heartbeat_interval = 60;
        s.no_ack = true;
    });
    let (gw, mut rx, _) = connected(&tether).await;
    next_event(&mut rx, |e| matches!(e, TetherEvent::Disconnected)).await;
    fake.with(|s| s.no_ack = false);
    next_event(&mut rx, |e| matches!(e, TetherEvent::Resumed)).await;
    assert!(fake.with(|s| s.connections >= 2 && !s.resumes.is_empty()));
    gw.shutdown().await;
}

#[tokio::test]
async fn a_dropped_connection_resumes_and_catches_up_on_the_game() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    gw.set_game(Some(GameActivity::plain(
        "Hollow Knight",
        1_760_000_000_000,
    )))
    .await
    .unwrap();
    fake.until("op 3", |s| s.op3.len() == 1).await;
    fake.with(|s| s.accept_resume = true);
    fake.cmd(ServerCmd::Drop);
    next_event(&mut rx, |e| matches!(e, TetherEvent::Disconnected)).await;
    // Changed while the connection was down.
    gw.set_game(Some(GameActivity::plain("Celeste", 1_760_000_000_000)))
        .await
        .unwrap();
    next_event(&mut rx, |e| matches!(e, TetherEvent::Resumed)).await;
    let resume = fake.with(|s| s.resumes[0].clone());
    assert_eq!(resume["session_id"], "session1");
    assert_eq!(resume["seq"], 1);
    assert!(resume["token"].as_str().unwrap().starts_with("tapp."));
    fake.until("the new game after resume", |s| s.op3.len() == 2)
        .await;
    assert_eq!(
        fake.with(|s| s.op3[1]["activities"][0]["name"].clone()),
        "Celeste"
    );
    assert_eq!(fake.with(|s| s.identifies.len()), 1);
    gw.shutdown().await;
}

#[tokio::test]
async fn invalid_session_identifies_again_and_resends_the_game() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    gw.set_game(Some(GameActivity::plain("Balatro", 1_760_000_000_000)))
        .await
        .unwrap();
    fake.until("op 3", |s| s.op3.len() == 1).await;
    fake.with(|s| s.accept_resume = false);
    fake.cmd(ServerCmd::Drop);
    next_event(&mut rx, |e| matches!(e, TetherEvent::Ready { .. })).await;
    assert_eq!(fake.with(|s| (s.resumes.len(), s.identifies.len())), (1, 2));
    fake.until("the game again on the new session", |s| s.op3.len() == 2)
        .await;
    assert_eq!(
        fake.with(|s| s.op3[1]["activities"][0]["name"].clone()),
        "Balatro"
    );
    gw.shutdown().await;
}

#[tokio::test]
async fn closes_that_end_the_session_identify_again() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    fake.cmd(ServerCmd::Close(4009));
    next_event(&mut rx, |e| matches!(e, TetherEvent::Ready { .. })).await;
    assert_eq!(fake.with(|s| (s.resumes.len(), s.identifies.len())), (0, 2));
    gw.shutdown().await;
}

#[tokio::test]
async fn a_4004_refreshes_the_token_and_identifies_with_the_new_one() {
    let (fake, tether, store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    let old = store.peek().unwrap().access_token;
    // The access token stopped working, the refresh token did not.
    fake.with(|s| s.access.values_mut().for_each(|v| *v = false));
    fake.cmd(ServerCmd::Close(4004));
    next_event(&mut rx, |e| matches!(e, TetherEvent::Ready { .. })).await;
    let new = store.peek().unwrap().access_token;
    assert_ne!(old, new);
    assert_eq!(
        fake.with(|s| s.identifies[1]["token"].as_str().unwrap().to_string()),
        new.expose()
    );
    gw.shutdown().await;
}

#[tokio::test]
async fn a_revoked_sign_in_stops_the_gateway_and_forgets_the_tokens() {
    let (fake, tether, store) = signed_in().await;
    let (_gw, mut rx, _) = connected(&tether).await;
    fake.revoke_all();
    fake.cmd(ServerCmd::Close(4004));
    next_event(&mut rx, |e| matches!(e, TetherEvent::SignedOut)).await;
    assert!(store.peek().is_none());
}

#[tokio::test]
async fn sign_out_closes_revokes_and_forgets() {
    let (fake, tether, store) = signed_in().await;
    let refresh = store.peek().unwrap().refresh_token;
    let (gw, _rx, _) = connected(&tether).await;
    tether.sign_out(Some(gw)).await.unwrap();
    assert_eq!(
        fake.with(|s| s.revoked.clone()),
        [refresh.expose().to_string()]
    );
    assert!(fake.with(|s| s.client_closes.contains(&1000)));
    assert!(store.peek().is_none());
    assert!(!tether.is_signed_in().await.unwrap());
}

#[tokio::test]
async fn without_a_sign_in_the_gateway_says_signed_out() {
    let (fake, tether, _store) = setup().await;
    let gw = tether.connect_with(options());
    let mut rx = gw.subscribe();
    next_event(&mut rx, |e| matches!(e, TetherEvent::SignedOut)).await;
    assert_eq!(fake.with(|s| s.connections), 0);
}

#[tokio::test]
async fn without_presence_game_no_op_3_is_ever_sent() {
    let (fake, tether, _store) = signed_in().await;
    fake.with(|s| s.scopes = vec!["identify", "friends.read"]);
    let (gw, _rx, _) = connected(&tether).await;
    gw.set_game(Some(GameActivity::plain("Tetris", 1_760_000_000_000)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(fake.with(|s| s.op3.is_empty()));
    gw.shutdown().await;
}

#[tokio::test]
async fn signing_in_again_revokes_the_old_sign_in() {
    let (fake, tether, store) = signed_in().await;
    let old = store.peek().unwrap().refresh_token;
    tether
        .sign_in(&FakeBrowser::new(fake.origin()))
        .await
        .unwrap();
    assert_eq!(fake.with(|s| s.revoked.clone()), [old.expose().to_string()]);
    assert_ne!(store.peek().unwrap().refresh_token, old);
}

// ---------------------------------------------------------------------------------------------- invites

const OPEN: &str = "2099-01-01T00:00:00.000Z";
const PAST: &str = "2000-01-01T00:00:00.000Z";

fn rocket_league() -> InviteGame {
    InviteGame::new("epic:Sugar", "Rocket League", None).unwrap()
}

/// An older tether (no game invites) is never asked for `invites`: it would refuse the whole sign-in.
#[tokio::test]
async fn sign_in_asks_for_invites_only_when_tether_has_them() {
    let (fake, tether, store) = setup().await;
    fake.with(|s| {
        s.invites_supported = false;
        s.scopes = vec!["identify", "presence.game", "friends.read"];
    });
    tether.sign_in(&FakeBrowser::new(fake.origin())).await.unwrap();
    assert_eq!(fake.with(|s| s.asked_scopes.clone()), ["identify presence.game friends.read"]);
    assert!(!tether.api().invites_supported().await);
    assert!(!store.peek().unwrap().scopes.contains(&"invites".to_string()));
    // Once tether has them, signing in again asks for them (the "Allow invites" re-consent).
    fake.with(|s| {
        s.invites_supported = true;
        s.scopes = vec!["identify", "presence.game", "friends.read", "invites"];
    });
    assert!(tether.api().invites_supported().await);
    tether.sign_in(&FakeBrowser::new(fake.origin())).await.unwrap();
    assert_eq!(fake.with(|s| s.asked_scopes[1].clone()), "identify presence.game friends.read invites");
    assert!(tether.scopes().await.unwrap().contains(&"invites".to_string()));
}

/// A sign-in made before invites existed has no `invites`: tether answers 403 50001, which is
/// "needs re-consent" (Allow invites), not a plain error.
#[tokio::test]
async fn a_sign_in_without_the_invites_scope_needs_consent() {
    let (fake, tether, _store) = setup().await;
    fake.with(|s| s.scopes = vec!["identify", "presence.game", "friends.read"]);
    tether.sign_in(&FakeBrowser::new(fake.origin())).await.unwrap();
    assert!(!tether.scopes().await.unwrap().contains(&"invites".to_string()));
    assert!(matches!(tether.api().send_invite("200", &rocket_league(), None).await, Err(Error::NeedsConsent)));
    assert!(matches!(tether.api().invites(50).await, Err(Error::NeedsConsent)));
    assert!(matches!(tether.api().invite_settings().await, Err(Error::NeedsConsent)));
    assert!(fake.with(|s| s.invites.is_empty()));
}

#[tokio::test]
async fn sends_lists_and_cancels_invites_with_gateway_events() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    assert!(gw.scopes().unwrap().contains(&"invites".to_string()));
    let sent = tether.api().send_invite("200", &rocket_league(), Some(900)).await.unwrap();
    assert_eq!((sent.sender_id.as_str(), sent.recipient_id.as_str(), sent.state), ("100", "200", InviteState::Sent));
    assert_eq!(sent.game.name, "Rocket League");
    // Exactly the contract's fields: nothing extra (tether answers 400 50035 to an unknown one).
    assert_eq!(fake.with(|s| s.invite_posts[0].clone()), json!({ "recipient_id": "200", "game": { "id": "epic:Sugar", "name": "Rocket League" }, "expires_in": 900 }));
    let TetherEvent::InviteCreated(created) = next_event(&mut rx, |e| matches!(e, TetherEvent::InviteCreated(_))).await else { unreachable!() };
    assert_eq!(created.id, sent.id);

    // A Steam game: the picture goes with it; no expiry named = tether's default.
    let cs2 = InviteGame::new("steam:730", "Counter-Strike 2", Some("steam:730")).unwrap();
    let second = tether.api().send_invite("300", &cs2, None).await.unwrap();
    assert_eq!(fake.with(|s| s.invite_posts[1].get("expires_in").cloned()), None);
    assert_eq!(second.game.image.as_deref(), Some("steam:730"));

    let list = tether.api().invites(50).await.unwrap();
    assert_eq!(list.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), [second.id.as_str(), sent.id.as_str()], "newest first");

    let cancelled = tether.api().cancel_invite(&sent.id).await.unwrap();
    assert_eq!(cancelled.state, InviteState::Cancelled);
    let TetherEvent::InviteUpdated(u) = next_event(&mut rx, |e| matches!(e, TetherEvent::InviteUpdated(_))).await else { unreachable!() };
    assert_eq!((u.id.as_str(), u.state), (sent.id.as_str(), InviteState::Cancelled));
    // Cancelling again changes nothing; the action routes send no body and no Content-Type.
    assert_eq!(tether.api().cancel_invite(&sent.id).await.unwrap().state, InviteState::Cancelled);
    assert!(fake.with(|s| s.invite_actions.iter().all(|(_, ct, len)| ct.is_none() && *len == 0)));
    // The sender cannot accept their own invite: with the scope held, 50001 means "the wrong side".
    assert!(matches!(tether.api().accept_invite(&second.id).await, Err(Error::NotAllowed)));
    gw.shutdown().await;
}

#[tokio::test]
async fn accepts_declines_and_reads_invites_sent_to_me() {
    let (fake, tether, _store) = signed_in().await;
    fake.seed_invite("111", "200", "100", "sent", OPEN, Some(json!({ "kind": "text", "value": "Private match: grapple / hunter2" })));
    fake.seed_invite("222", "300", "100", "sent", OPEN, None);
    fake.seed_invite("333", "200", "100", "sent", PAST, None);
    fake.seed_invite("444", "200", "300", "sent", OPEN, None); // between two other people

    // Shown in Grapple: seen. The recipient's read carries the join payload.
    assert_eq!(tether.api().invite_seen("111").await.unwrap().state, InviteState::Seen);
    let (invite, join) = tether.api().invite("111").await.unwrap();
    assert_eq!(invite.state, InviteState::Seen);
    assert!(invite.joinable);
    let Some(InviteJoin::Text(text)) = join else { panic!("a text payload") };
    assert_eq!(text.expose(), "Private match: grapple / hunter2");

    let accepted = tether.api().accept_invite("111").await.unwrap();
    assert_eq!(accepted.invite.state, InviteState::Accepted);
    assert!(!accepted.app_notified, "accepted by Grapple itself: Grapple acts, tether tells nobody");
    // Play again before expires_at is fine; declining after accepting is not.
    assert_eq!(tether.api().accept_invite("111").await.unwrap().invite.state, InviteState::Accepted);
    assert!(matches!(tether.api().decline_invite("111").await, Err(Error::InviteClosed)));

    assert_eq!(tether.api().decline_invite("222").await.unwrap().state, InviteState::Declined);
    assert!(matches!(tether.api().accept_invite("222").await, Err(Error::InviteClosed)));
    // Past its time: 400 90271.
    assert!(matches!(tether.api().accept_invite("333").await, Err(Error::InviteClosed)));
    // Someone else's, or no such invite: 404 90270 ("isn't for this account").
    assert!(matches!(tether.api().invite("444").await, Err(Error::UnknownInvite)));
    assert!(matches!(tether.api().accept_invite("999").await, Err(Error::UnknownInvite)));
    // The recipient cannot cancel.
    assert!(matches!(tether.api().cancel_invite("222").await, Err(Error::NotAllowed)));
    assert_eq!(fake.invite("222")["state"], "declined");
}

#[tokio::test]
async fn invite_errors_are_the_contracts() {
    let (fake, tether, _store) = signed_in().await;
    let api = tether.api();
    assert!(matches!(api.send_invite("400", &rocket_league(), None).await, Err(Error::CannotInvite)));
    assert!(matches!(api.send_invite("100", &rocket_league(), None).await, Err(Error::UnknownUser)));
    assert!(matches!(api.send_invite("12345", &rocket_league(), None).await, Err(Error::UnknownUser)));
    fake.with(|s| s.sends_left = Some(0));
    match api.send_invite("200", &rocket_league(), None).await {
        Err(Error::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 30),
        other => panic!("expected a rate limit, got {other:?}"),
    }
    // Nothing that is not a snowflake ever goes into a path, and no request is made for it.
    let before = fake.with(|s| s.invite_actions.len());
    for bad in ["", "../users", "12a", "1/accept", &"9".repeat(21)] {
        assert!(matches!(api.accept_invite(bad).await, Err(Error::UnknownInvite)), "{bad}");
        assert!(matches!(api.invite(bad).await, Err(Error::UnknownInvite)), "{bad}");
    }
    assert_eq!(fake.with(|s| s.invite_actions.len()), before);
    assert!(matches!(api.send_invite("2a", &rocket_league(), None).await, Err(Error::UnknownUser)));
    assert!(matches!(InviteGame::new("gog:1", "x", None), Err(Error::Invalid(_))));
}

#[tokio::test]
async fn who_can_invite_me_is_read_and_set() {
    let (fake, tether, _store) = signed_in().await;
    assert_eq!(tether.api().invite_settings().await.unwrap(), InvitesFrom::Friends);
    assert_eq!(tether.api().set_invite_settings(InvitesFrom::Nobody).await.unwrap(), InvitesFrom::Nobody);
    assert_eq!(fake.with(|s| s.invites_from.clone()), "nobody");
    assert_eq!(tether.api().invite_settings().await.unwrap(), InvitesFrom::Nobody);
}

#[tokio::test]
async fn play_events_carry_the_invite_and_only_known_join_kinds() {
    let (fake, tether, _store) = signed_in().await;
    let (gw, mut rx, _) = connected(&tether).await;
    let invite = json!({
        "id": "555", "channel_id": "9", "message_id": "555", "sender_id": "200", "recipient_id": "100",
        "game": { "id": "steam:730", "name": "Counter-Strike 2", "image": null }, "state": "accepted", "joinable": true,
        "created_at": "2026-11-25T18:00:00.000Z", "expires_at": "2026-11-25T18:15:00.000Z",
        "seen_at": null, "answered_at": "2026-11-25T18:01:00.000Z", "a_future_field": { "x": 1 }
    });
    fake.cmd(ServerCmd::Send("GAME_INVITE_PLAY", json!({ "invite": invite, "join": { "kind": "text", "value": "connect 10.0.0.2" } })));
    let TetherEvent::InvitePlay { invite: got, join } = next_event(&mut rx, |e| matches!(e, TetherEvent::InvitePlay { .. })).await else { unreachable!() };
    assert_eq!((got.id.as_str(), got.state, got.game.id.as_str()), ("555", InviteState::Accepted, "steam:730"));
    assert!(matches!(&join, Some(InviteJoin::Text(t)) if t.expose() == "connect 10.0.0.2"));
    assert!(!format!("{join:?}").contains("10.0.0.2"), "a payload never shows in Debug output");
    // A kind this Grapple does not know is no payload; a play without one has none.
    fake.cmd(ServerCmd::Send("GAME_INVITE_PLAY", json!({ "invite": invite, "join": { "kind": "steam_lobby", "lobby": "1" } })));
    let TetherEvent::InvitePlay { join, .. } = next_event(&mut rx, |e| matches!(e, TetherEvent::InvitePlay { .. })).await else { unreachable!() };
    assert_eq!(join, None);
    fake.cmd(ServerCmd::Send("GAME_INVITE_UPDATE", json!({ "invite": { "id": "broken" } })));
    fake.cmd(ServerCmd::Send("GAME_INVITE_UPDATE", json!({ "invite": invite })));
    let TetherEvent::InviteUpdated(u) = next_event(&mut rx, |e| matches!(e, TetherEvent::InviteUpdated(_))).await else { unreachable!() };
    assert_eq!(u.id, "555", "an unreadable invite is dropped, the next one still arrives");
    gw.shutdown().await;
}
