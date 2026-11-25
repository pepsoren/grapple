//! Opt-in end-to-end run against a LOCAL tether from the `grapple-apps` branch (`pnpm dev:lite
//! --seed`), never a real instance. Ignored by default. Run with:
//!
//!   GRAPPLE_TETHER_DEV_URL=http://localhost:5311 GRAPPLE_TETHER_DEV_PASSWORD=password \
//!     nice -n 19 cargo test -p grapple-tether --test dev_lite -- --ignored
//!
//! It signs the seeded `josh` and `rowan` in through the real authorize API (standing in for the
//! browser: the person's session answers Allow), connects both as Grapple, has josh play a game
//! and checks rowan's Grapple sees it, uploads an icon, and signs both out.

use grapple_tether::{
    Browser, Error, GameActivity, MemoryStore, Status, Tether, TetherConfig, TetherEvent,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// A 1×1 PNG (a valid file that tether's decoder accepts).
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Answers the authorize request with a person's session, the way the web page's Allow does.
struct SessionBrowser {
    origin: String,
    session: String,
}

impl Browser for SessionBrowser {
    fn open(&self, url: &url::Url) -> Result<(), Error> {
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let (origin, session) = (self.origin.clone(), self.session.clone());
        tokio::spawn(async move {
            let http = reqwest::Client::new();
            let mut check_url =
                url::Url::parse(&format!("{origin}/api/v1/oauth2/authorize")).unwrap();
            check_url.query_pairs_mut().extend_pairs(q.iter());
            let check = http
                .get(check_url)
                .bearer_auth(&session)
                .send()
                .await
                .unwrap();
            assert_eq!(check.status(), 200, "the server accepted Grapple's request");
            let mut body: serde_json::Map<String, Value> =
                q.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
            body.insert("authorize".into(), json!(true));
            let answer: Value = http
                .post(format!("{origin}/api/v1/oauth2/authorize"))
                .bearer_auth(&session)
                .json(&body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let res = http
                .get(answer["location"].as_str().unwrap())
                .send()
                .await
                .unwrap();
            assert!(res.status().is_success());
        });
        Ok(())
    }
}

async fn session_of(origin: &str, login: &str, password: &str) -> String {
    let res: Value = reqwest::Client::new()
        .post(format!("{origin}/api/v1/auth/login"))
        .json(&json!({ "login": login, "password": password }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res["token"]
        .as_str()
        .expect("logged in to the local dev server")
        .to_string()
}

async fn grapple_for(origin: &str, login: &str, password: &str) -> Tether {
    let config = TetherConfig::new(origin).unwrap();
    assert!(
        config.origin().starts_with("http://localhost")
            || config.origin().starts_with("http://127.0.0.1"),
        "this test only ever runs against a local dev server"
    );
    let tether = Tether::new(config, Arc::new(MemoryStore::new())).unwrap();
    let session = session_of(origin, login, password).await;
    let me = tether
        .sign_in(&SessionBrowser {
            origin: origin.into(),
            session,
        })
        .await
        .unwrap();
    assert_eq!(me.username, login);
    tether
}

#[tokio::test]
#[ignore = "needs a local dev:lite from the grapple-apps branch"]
async fn end_to_end_against_local_dev_lite() {
    let origin = std::env::var("GRAPPLE_TETHER_DEV_URL").expect("GRAPPLE_TETHER_DEV_URL");
    let password =
        std::env::var("GRAPPLE_TETHER_DEV_PASSWORD").expect("GRAPPLE_TETHER_DEV_PASSWORD");
    let josh = grapple_for(&origin, "josh", &password).await;
    let rowan = grapple_for(&origin, "rowan", &password).await;

    let josh_gw = josh.connect();
    let rowan_gw = rowan.connect();
    let mut rowan_rx = rowan_gw.subscribe();
    let mut josh_rx = josh_gw.subscribe();
    let wait = |rx: &mut tokio::sync::broadcast::Receiver<TetherEvent>,
                pred: Box<dyn Fn(&TetherEvent) -> bool>| {
        let mut rx = rx.resubscribe();
        async move {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let e = rx.recv().await.unwrap();
                    if pred(&e) {
                        return e;
                    }
                }
            })
            .await
            .expect("event in time")
        }
    };
    let ready = wait(
        &mut josh_rx,
        Box::new(|e| matches!(e, TetherEvent::Ready { .. })),
    )
    .await;
    let TetherEvent::Ready { friends, .. } = ready else {
        unreachable!()
    };
    assert!(friends.iter().any(|f| f.user.username == "rowan"));
    wait(
        &mut rowan_rx,
        Box::new(|e| matches!(e, TetherEvent::Ready { .. })),
    )
    .await;

    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
        - 30_000;
    let seen = wait(
        &mut rowan_rx,
        Box::new(
            |e| matches!(e, TetherEvent::Friend(p) if p.user.username == "josh" && p.game.is_some()),
        ),
    );
    josh_gw
        .set_game(Some(GameActivity::with_icon(
            "Grapple test game",
            PNG_1X1.to_vec(),
            started,
        )))
        .await
        .unwrap();
    let TetherEvent::Friend(p) = seen.await else {
        unreachable!()
    };
    assert_eq!(p.status, Status::Online);
    let game = p.game.unwrap();
    assert_eq!(game.name, "Grapple test game");
    assert_eq!(game.since_ms, started);
    let image = game.image.expect("the uploaded icon");
    assert!(
        rowan.api().game_icon(&image).await.unwrap().is_some(),
        "rowan's Grapple can fetch the picture"
    );

    let gone = wait(
        &mut rowan_rx,
        Box::new(
            |e| matches!(e, TetherEvent::Friend(p) if p.user.username == "josh" && p.status == Status::Offline),
        ),
    );
    josh_gw.set_game(None).await.unwrap();
    gone.await;

    josh.sign_out(Some(josh_gw)).await.unwrap();
    rowan.sign_out(Some(rowan_gw)).await.unwrap();
    assert!(matches!(josh.api().me().await, Err(Error::NotSignedIn)));
}
