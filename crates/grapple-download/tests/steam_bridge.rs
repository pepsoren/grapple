//! The Steam bridge against a fake Chrome DevTools endpoint on 127.0.0.1 (axum + WebSocket). No Steam.

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::routing::get;
use grapple_download::steam_bridge::{self, BridgeEvent, Call, Ns, SteamBridge, Status, discover, is_template};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

#[derive(Clone)]
struct Fake {
    title: String,
    page_url: String,
    ws_host: Option<String>,
    browser: String,
    downloads: Vec<&'static str>,
    /// Which call makes a download show up: "continue" or "wizard".
    starts_on: &'static str,
    seen: Seen,
}

impl Default for Fake {
    fn default() -> Self {
        Fake {
            title: "SharedJSContext".into(),
            page_url: "https://steamloopback.host/routes/".into(),
            ws_host: None,
            browser: "Chrome/126.0.6478.183".into(),
            downloads: vec![
                "EnableAllDownloads", "PauseAppUpdate", "ResumeAppUpdate", "QueueAppUpdate", "SetQueueIndex", "MoveAppUpdateUp", "MoveAppUpdateDown",
                "RemoveFromDownloadList", "RegisterForDownloadItems", "RegisterForDownloadOverview",
            ],
            starts_on: "continue",
            seen: Arc::default(),
        }
    }
}

async fn start(f: Fake) -> (u16, Fake) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let st = f.clone();
    let app = Router::new()
        .route("/json/version", get(|State(f): State<Fake>| async move { axum::Json(json!({"Browser": f.browser, "Protocol-Version": "1.3"})) }))
        .route(
            "/json",
            get(move |State(f): State<Fake>| async move {
                let host = f.ws_host.clone().unwrap_or_else(|| format!("127.0.0.1:{port}"));
                axum::Json(json!([
                    {"title": "Steam", "type": "page", "url": "https://steamloopback.host/index.html", "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/OTHER")},
                    {"title": f.title, "type": "page", "url": f.page_url, "webSocketDebuggerUrl": format!("ws://{host}/devtools/page/SJC")}
                ]))
            }),
        )
        .route("/devtools/page/SJC", get(|ws: WebSocketUpgrade, State(f): State<Fake>| async move { ws.on_upgrade(move |s| cdp(s, f)) }))
        .with_state(st);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, f)
}

fn binding(payload: Value) -> String {
    json!({"method": "Runtime.bindingCalled", "params": {"name": "grappleDownloads", "payload": payload.to_string(), "executionContextId": 1}}).to_string()
}

fn items(appid: u32) -> Value {
    json!({"kind": "items", "d": true, "items": [{"appid": appid, "active": true, "paused": false, "completed": false, "queue_index": 0,
        "downloaded_bytes": 1000, "total_bytes": 4000, "buildid": 0, "target_buildid": 9, "update_error": ""}]})
}

async fn cdp(mut s: WebSocket, f: Fake) {
    while let Some(Ok(Message::Text(t))) = s.recv().await {
        let v: Value = serde_json::from_str(&t).unwrap();
        let id = v["id"].clone();
        let method = v["method"].as_str().unwrap_or("").to_string();
        let expr = v.pointer("/params/expression").and_then(Value::as_str).map(str::to_string);
        f.seen.lock().unwrap().push((method.clone(), expr.clone()));
        let e = expr.unwrap_or_default();
        let mut after: Vec<String> = Vec::new();
        let value = if e.contains("Object.keys(SteamClient.Downloads)") {
            json!(f.downloads)
        } else if e.contains("Object.keys(SteamClient.Installs)") {
            json!(["SetAppList", "SetInstallFolder", "ContinueInstall", "CancelInstall", "GetInstallManagerInfo", "OpenInstallWizard", "RegisterForShowInstallWizard", "OpenUninstallWizard"])
        } else if e.contains("Object.keys(SteamClient.InstallFolder)") {
            json!(["GetInstallFolders"])
        } else if e.contains("GetInstallFolders()") {
            json!([{"nFolderIndex": 0, "strFolderPath": "/home/j/.local/share/Steam", "bIsDefaultFolder": true}, {"nFolderIndex": 1, "strFolderPath": "/data/SteamLibrary", "bIsDefaultFolder": false}])
        } else if e.contains("GetInstallManagerInfo()") {
            json!({"rgAppIDs": [{"nAppID": 252490, "lDiskSpaceRequiredBytes": 40_000_000_000u64}], "nDiskSpaceRequired": 40_000_000_000u64})
        } else if e.contains("RegisterForDownloadItems") {
            after.push(binding(json!({"kind": "items", "d": false, "items": []})));
            json!(true)
        } else if e.contains("RegisterForDownloadOverview") {
            after.push(binding(json!({"kind": "overview", "o": {"update_appid": 252490, "update_state": "Updating", "paused": false, "update_bytes_downloaded": 1000,
                "update_bytes_to_download": 4000, "update_network_bytes_per_second": 42800000, "update_seconds_remaining": 540, "update_is_install": true}})));
            json!(true)
        } else if (e.contains("ContinueInstall()") && !e.contains("RegisterForShowInstallWizard") && f.starts_on == "continue")
            || (e.contains("OpenInstallWizard") && f.starts_on == "wizard")
        {
            after.push(binding(items(252490)));
            Value::Null
        } else if e.contains("PauseAppUpdate(13)") {
            json!({"error": "TypeError: x"})
        } else {
            Value::Null
        };
        let resp = if method == "Runtime.evaluate" { json!({"id": id, "result": {"result": {"type": "object", "value": value}}}) } else { json!({"id": id, "result": {}}) };
        if s.send(Message::Text(resp.to_string().into())).await.is_err() {
            break;
        }
        for a in after {
            let _ = s.send(Message::Text(a.into())).await;
        }
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

#[tokio::test]
async fn only_the_steam_page_on_loopback_is_accepted() {
    let (port, _) = start(Fake::default()).await;
    let ws = discover(&http(), port).await.unwrap();
    assert!(ws.starts_with(&format!("ws://127.0.0.1:{port}/")) && ws.ends_with("/SJC"));

    let (port, _) = start(Fake { title: "Steam Big Picture".into(), ..Fake::default() }).await;
    assert!(discover(&http(), port).await.is_err(), "wrong title");
    let (port, _) = start(Fake { page_url: "https://evil.example/routes/".into(), ..Fake::default() }).await;
    assert!(discover(&http(), port).await.is_err(), "wrong page host");
    let (port, _) = start(Fake { ws_host: Some("192.168.1.20:8080".into()), ..Fake::default() }).await;
    assert!(discover(&http(), port).await.is_err(), "non-loopback websocket");
    let (port, _) = start(Fake { browser: "SomethingElse/1.0".into(), ..Fake::default() }).await;
    assert!(discover(&http(), port).await.is_err(), "not Chrome");
    // Nothing listening.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    assert!(discover(&http(), free).await.is_err());
}

async fn connected(port: u16) -> (SteamBridge, mpsc::UnboundedReceiver<BridgeEvent>, Vec<BridgeEvent>) {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let b = SteamBridge::spawn(port, tx);
    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("bridge connects").unwrap();
        let done = ev == BridgeEvent::Connected;
        events.push(ev);
        if done {
            break;
        }
    }
    assert_eq!(b.status(), Status::Connected);
    (b, rx, events)
}

#[tokio::test]
async fn calls_events_and_install() {
    let (port, f) = start(Fake::default()).await;
    let (b, mut rx, early) = connected(port).await;
    // Registration events arrive: the (empty) list and the overview.
    let mut got_overview = early.into_iter().find_map(|e| if let BridgeEvent::Overview(o) = e { Some(o) } else { None });
    for _ in 0..4 {
        if got_overview.is_some() {
            break;
        }
        if let Ok(Some(BridgeEvent::Overview(o))) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            got_overview = Some(o);
        }
    }
    let o = got_overview.expect("overview event");
    assert_eq!((o.appid, o.bytes_per_sec, o.seconds_left, o.to_download), (252490, 42_800_000, 540, 4000));

    b.call(Call::EnableAllDownloads(false)).await.unwrap();
    b.call(Call::SetQueueIndex(892970, 0)).await.unwrap();
    assert!(matches!(b.call(Call::PauseAppUpdate(13)).await, Err(grapple_download::Error::Steam(_))), "Steam's own error comes back");
    assert_eq!(b.install_size(252490).await.unwrap(), Some(40_000_000_000));
    let keys = vec![(0, "/home/j/.local/share/Steam".into()), (1, "/data/SteamLibrary".into())];
    assert_eq!(b.folder_index(std::path::Path::new("/data/SteamLibrary"), &keys).await, Some(1));
    assert_eq!(b.folder_index(std::path::Path::new("/elsewhere"), &keys).await, Some(0), "unsure: Steam's default");
    b.install(252490, 1, Duration::from_secs(2)).await.unwrap();
    assert!(b.items().iter().any(|i| i.appid == 252490 && i.active && i.total == 4000));

    let seen = f.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|(m, _)| m == "Runtime.addBinding"));
    let exprs: Vec<&String> = seen.iter().filter_map(|(_, e)| e.as_ref()).collect();
    assert!(exprs.len() >= 10);
    for e in &exprs {
        assert!(is_template(e), "not a template: {e}");
    }
    // The no-window install sequence, in order.
    let i = exprs.iter().position(|e| e.contains("SetInstallFolder(1)")).unwrap();
    assert!(exprs[i - 1].contains("SetAppList([252490])") && exprs[i + 1].contains("ContinueInstall()"));
    b.stop();
}

#[tokio::test]
async fn missing_functions_are_never_called_and_the_wizard_is_the_fallback() {
    let mut f = Fake { starts_on: "wizard", ..Fake::default() };
    f.downloads.retain(|d| *d != "PauseAppUpdate");
    let (port, f) = start(f).await;
    let (b, _rx, _) = connected(port).await;
    assert!(!b.has(Call::PauseAppUpdate(1)));
    assert!(b.call(Call::PauseAppUpdate(1)).await.is_err());
    assert!(!f.seen.lock().unwrap().iter().any(|(_, e)| e.as_deref().is_some_and(|e| e.contains("PauseAppUpdate"))));
    b.install(252490, 0, Duration::from_millis(400)).await.unwrap();
    let seen = f.seen.lock().unwrap().clone();
    let exprs: Vec<&String> = seen.iter().filter_map(|(_, e)| e.as_ref()).collect();
    let w = exprs.iter().position(|e| e.contains("RegisterForShowInstallWizard")).expect("wizard callback registered");
    assert!(exprs[w + 1].contains("OpenInstallWizard([252490])"));
    assert!(exprs.iter().all(|e| is_template(e)));
}

#[test]
fn template_list_covers_every_call() {
    let t = steam_bridge::templates();
    assert_eq!(t.len(), 22);
    assert!(is_template(&Call::Keys(Ns::Installs).expression()));
    assert!(is_template(&Call::ContinueWizardOnce(3).expression()));
}
