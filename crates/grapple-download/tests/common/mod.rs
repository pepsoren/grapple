//! A fake Epic on 127.0.0.1: the launcher service (owned games, manifest location) and two CDNs. No real Epic, no
//! real login file: the token is a fixture in a temp `user.json` that never expires.
#![allow(dead_code)]

use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use grapple_download::manifest::{hex, sha1};
use grapple_download::testkit::Built;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const TOKEN: &str = "fake-access-token";

#[derive(Default)]
pub struct EpicState {
    /// app name → (namespace, catalog id, build).
    pub games: HashMap<String, (String, String, Arc<Built>)>,
    /// Chunk paths CDN 1 answers 500 for.
    pub cdn1_fails: HashSet<String>,
    /// Chunk paths served corrupt once (then fixed).
    pub corrupt_once: HashSet<String>,
    /// Every chunk request: (cdn, path).
    pub chunk_requests: Vec<(u8, String)>,
    /// A chunk request carried an Authorization header.
    pub chunk_auth_seen: bool,
    pub service_auth: Vec<String>,
    pub signed: bool,
    pub with_query: bool,
}

pub type Shared = Arc<Mutex<EpicState>>;

pub struct FakeEpic {
    pub addr: SocketAddr,
    pub state: Shared,
}

impl FakeEpic {
    pub async fn start() -> FakeEpic {
        let state: Shared = Arc::new(Mutex::new(EpicState { with_query: true, ..EpicState::default() }));
        let app = Router::new()
            .route("/launcher/api/public/assets/Windows", get(assets))
            .route("/launcher/api/public/assets/v2/platform/Windows/namespace/{ns}/catalogItem/{cat}/app/{app}/label/Live", get(location))
            .route("/c/{n}/Builds/{app}/{*rest}", get(cdn))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        FakeEpic { addr, state }
    }

    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn add(&self, app: &str, built: Built) {
        self.state.lock().unwrap().games.insert(app.into(), ("ns1".into(), "cat1".into(), Arc::new(built)));
    }

    pub fn requests(&self) -> Vec<(u8, String)> {
        self.state.lock().unwrap().chunk_requests.clone()
    }
}

fn authed(h: &HeaderMap, st: &Shared) -> bool {
    let v = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    st.lock().unwrap().service_auth.push(v.clone());
    v.eq_ignore_ascii_case(&format!("bearer {TOKEN}"))
}

async fn assets(State(st): State<Shared>, h: HeaderMap) -> Response {
    if !authed(&h, &st) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let s = st.lock().unwrap();
    let list: Vec<Value> = s
        .games
        .iter()
        .map(|(app, (ns, cat, b))| json!({"appName": app, "labelName": "Live-Windows", "buildVersion": b.manifest.meta.build_version, "catalogItemId": cat, "namespace": ns, "assetId": app}))
        .collect();
    axum::Json(list).into_response()
}

async fn location(State(st): State<Shared>, h: HeaderMap, AxPath((_ns, _cat, app)): AxPath<(String, String, String)>) -> Response {
    if !authed(&h, &st) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let s = st.lock().unwrap();
    let Some((_, _, b)) = s.games.get(&app) else { return StatusCode::NOT_FOUND.into_response() };
    let host = h.get("host").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let q = if s.with_query { json!([{"name": "f_token", "value": "abc"}]) } else { json!([]) };
    json!({"elements": [{"appName": app, "labelName": "Live", "buildVersion": b.manifest.meta.build_version, "hash": hex(&sha1(&b.manifest_bytes)), "useSignedUrl": s.signed,
        "manifests": [
            {"uri": format!("http://{host}/c/1/Builds/{app}/default/{}.manifest", b.manifest.meta.build_version), "queryParams": q},
            {"uri": format!("http://{host}/c/2/Builds/{app}/default/{}.manifest", b.manifest.meta.build_version), "queryParams": []}
        ]}]})
    .to_string()
    .into_response()
}

async fn cdn(State(st): State<Shared>, h: HeaderMap, AxPath((n, app, rest)): AxPath<(u8, String, String)>) -> Response {
    let mut s = st.lock().unwrap();
    let Some((_, _, b)) = s.games.get(&app).cloned() else { return StatusCode::NOT_FOUND.into_response() };
    let rest = rest.trim_start_matches("default/").to_string();
    if rest.ends_with(".manifest") {
        return b.manifest_bytes.clone().into_response();
    }
    if h.contains_key("authorization") {
        s.chunk_auth_seen = true;
    }
    s.chunk_requests.push((n, rest.clone()));
    if n == 1 && s.cdn1_fails.contains(&rest) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let Some(data) = b.chunks.get(&rest) else { return StatusCode::NOT_FOUND.into_response() };
    let mut data = data.clone();
    if s.corrupt_once.remove(&rest) {
        let at = data.len() - 1;
        data[at] ^= 0x55;
    }
    data.into_response()
}

/// A temp `user.json` with a fixture token valid for years (EpicAuth::session() returns it without a network call).
pub fn fake_login(dir: &Path) -> PathBuf {
    let p = dir.join("user.json");
    std::fs::write(&p, json!({"access_token": TOKEN, "expires_at": "2099-01-01T00:00:00.000Z", "refresh_token": "r", "account_id": "acc", "displayName": "tester"}).to_string()).unwrap();
    p
}

/// legendary's metadata file for a game.
pub fn write_metadata(legendary: &Path, app: &str, title: &str, folder: Option<&str>) {
    std::fs::create_dir_all(legendary.join("metadata")).unwrap();
    let mut attrs = json!({"CanRunOffline": {"type": "STRING", "value": "true"}, "OwnershipToken": {"type": "STRING", "value": "false"}});
    if let Some(f) = folder {
        attrs["FolderName"] = json!({"type": "STRING", "value": f});
    }
    let v = json!({"app_name": app, "app_title": title, "asset_infos": {"Windows": {"app_name": app, "namespace": "ns1", "catalog_item_id": "cat1", "build_version": "x"}},
        "metadata": {"namespace": "ns1", "id": "cat1", "title": title, "customAttributes": attrs, "categories": [{"path": "games"}]}});
    std::fs::write(legendary.join("metadata").join(format!("{app}.json")), v.to_string()).unwrap();
}

/// Every file of a built game, as (relative path, bytes), for comparing with the install folder.
pub fn expected_files(b: &Built, spec_files: &[grapple_download::testkit::TestFile]) -> Vec<(String, Vec<u8>)> {
    let _ = b;
    spec_files.iter().filter(|f| f.symlink.is_none()).map(|f| (f.name.clone(), f.data.clone())).collect()
}
