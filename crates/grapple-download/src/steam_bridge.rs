//! The opt-in Steam bridge: Steam's own UI is a Chromium app; with the empty file
//! `<steam root>/.cef-enable-remote-debugging` present when Steam starts, it serves the Chrome DevTools Protocol on
//! `127.0.0.1:8080`. Its `SharedJSContext` page has the global `SteamClient` object (Decky Loader and Millennium use
//! the same). Through it grapple pauses, resumes, reorders, installs and uninstalls Steam games with no Steam window.
//!
//! Safety rules (docs/DOWNLOADS_PLAN.md):
//! * connect only to loopback, and only to a `SharedJSContext` target whose page is on `steamloopback.host`;
//! * evaluate only the fixed expressions of [`Call`], whose only inputs are integers and booleans; every expression
//!   is checked against the template list before it is sent ([`is_template`]);
//! * never log the websocket URL.

use crate::error::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

pub const DEFAULT_PORT: u16 = 8080;

/// A loopback port nobody listens on now (the OS picks it), for Steam's `-devtools-port`.
pub fn free_loopback_port() -> Result<u16> {
    let l = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| Error::Steam(format!("no free port: {e}")))?;
    l.local_addr().map(|a| a.port()).map_err(|e| Error::Steam(format!("no free port: {e}")))
}
pub const ENABLE_FILE: &str = ".cef-enable-remote-debugging";
const BINDING: &str = "grappleDownloads";

/// Every call grapple makes into Steam. Arguments are integers or booleans only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    /// `Object.keys(SteamClient.<ns>)`: which functions this Steam has.
    Keys(Ns),
    EnableAllDownloads(bool),
    PauseAppUpdate(u32),
    ResumeAppUpdate(u32),
    QueueAppUpdate(u32),
    SetQueueIndex(u32, u32),
    MoveAppUpdateUp(u32),
    MoveAppUpdateDown(u32),
    RemoveFromDownloadList(u32),
    RegisterDownloadItems,
    RegisterDownloadOverview,
    SetAppList(u32),
    SetInstallFolder(u32),
    ContinueInstall,
    CancelInstall,
    GetInstallManagerInfo,
    OpenInstallWizard(u32),
    /// One-shot: when Steam shows its install wizard, pick the folder and continue.
    ContinueWizardOnce(u32),
    OpenUninstallWizard(u32),
    GetInstallFolders,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ns {
    Downloads,
    Installs,
    InstallFolder,
}

impl Ns {
    pub fn name(self) -> &'static str {
        match self {
            Ns::Downloads => "Downloads",
            Ns::Installs => "Installs",
            Ns::InstallFolder => "InstallFolder",
        }
    }
}

impl Call {
    /// The namespace and function a call needs (for the feature check).
    pub fn needs(&self) -> Option<(Ns, &'static str)> {
        use Call::*;
        Some(match self {
            Keys(_) => return None,
            EnableAllDownloads(_) => (Ns::Downloads, "EnableAllDownloads"),
            PauseAppUpdate(_) => (Ns::Downloads, "PauseAppUpdate"),
            ResumeAppUpdate(_) => (Ns::Downloads, "ResumeAppUpdate"),
            QueueAppUpdate(_) => (Ns::Downloads, "QueueAppUpdate"),
            SetQueueIndex(..) => (Ns::Downloads, "SetQueueIndex"),
            MoveAppUpdateUp(_) => (Ns::Downloads, "MoveAppUpdateUp"),
            MoveAppUpdateDown(_) => (Ns::Downloads, "MoveAppUpdateDown"),
            RemoveFromDownloadList(_) => (Ns::Downloads, "RemoveFromDownloadList"),
            RegisterDownloadItems => (Ns::Downloads, "RegisterForDownloadItems"),
            RegisterDownloadOverview => (Ns::Downloads, "RegisterForDownloadOverview"),
            SetAppList(_) => (Ns::Installs, "SetAppList"),
            SetInstallFolder(_) => (Ns::Installs, "SetInstallFolder"),
            ContinueInstall => (Ns::Installs, "ContinueInstall"),
            CancelInstall => (Ns::Installs, "CancelInstall"),
            GetInstallManagerInfo => (Ns::Installs, "GetInstallManagerInfo"),
            OpenInstallWizard(_) => (Ns::Installs, "OpenInstallWizard"),
            ContinueWizardOnce(_) => (Ns::Installs, "RegisterForShowInstallWizard"),
            OpenUninstallWizard(_) => (Ns::Installs, "OpenUninstallWizard"),
            GetInstallFolders => (Ns::InstallFolder, "GetInstallFolders"),
        })
    }

    fn body(&self) -> String {
        use Call::*;
        let d = "SteamClient.Downloads";
        let i = "SteamClient.Installs";
        match *self {
            Keys(ns) => format!("Object.keys(SteamClient.{})", ns.name()),
            EnableAllDownloads(on) => format!("{d}.EnableAllDownloads({on})"),
            PauseAppUpdate(a) => format!("{d}.PauseAppUpdate({a})"),
            ResumeAppUpdate(a) => format!("{d}.ResumeAppUpdate({a})"),
            QueueAppUpdate(a) => format!("{d}.QueueAppUpdate({a})"),
            SetQueueIndex(a, n) => format!("{d}.SetQueueIndex({a}, {n})"),
            MoveAppUpdateUp(a) => format!("{d}.MoveAppUpdateUp({a})"),
            MoveAppUpdateDown(a) => format!("{d}.MoveAppUpdateDown({a})"),
            RemoveFromDownloadList(a) => format!("{d}.RemoveFromDownloadList({a})"),
            RegisterDownloadItems => {
                format!("({d}.RegisterForDownloadItems((d, items) => {BINDING}(JSON.stringify({{kind: \"items\", d, items}}))), true)")
            }
            RegisterDownloadOverview => format!("({d}.RegisterForDownloadOverview((o) => {BINDING}(JSON.stringify({{kind: \"overview\", o}}))), true)"),
            SetAppList(a) => format!("{i}.SetAppList([{a}])"),
            SetInstallFolder(n) => format!("{i}.SetInstallFolder({n})"),
            ContinueInstall => format!("{i}.ContinueInstall()"),
            CancelInstall => format!("{i}.CancelInstall()"),
            GetInstallManagerInfo => format!("await {i}.GetInstallManagerInfo()"),
            OpenInstallWizard(a) => format!("{i}.OpenInstallWizard([{a}])"),
            ContinueWizardOnce(n) => format!(
                "(() => {{ const h = {i}.RegisterForShowInstallWizard(() => {{ try {{ {i}.SetInstallFolder({n}); {i}.ContinueInstall(); }} finally {{ if (h && h.unregister) h.unregister(); }} }}); return true; }})()"
            ),
            OpenUninstallWizard(a) => format!("{i}.OpenUninstallWizard([{a}], true)"),
            GetInstallFolders => "await SteamClient.InstallFolder.GetInstallFolders()".to_string(),
        }
    }

    /// The full expression sent to `Runtime.evaluate`.
    pub fn expression(&self) -> String {
        format!("(async () => {{ try {{ return {}; }} catch (e) {{ return ({{error: String(e)}}); }} }})()", self.body())
    }

    /// One of each, for the template list.
    fn samples() -> Vec<Call> {
        use Call::*;
        vec![
            Keys(Ns::Downloads),
            Keys(Ns::Installs),
            Keys(Ns::InstallFolder),
            EnableAllDownloads(true),
            PauseAppUpdate(0),
            ResumeAppUpdate(0),
            QueueAppUpdate(0),
            SetQueueIndex(0, 0),
            MoveAppUpdateUp(0),
            MoveAppUpdateDown(0),
            RemoveFromDownloadList(0),
            RegisterDownloadItems,
            RegisterDownloadOverview,
            SetAppList(0),
            SetInstallFolder(0),
            ContinueInstall,
            CancelInstall,
            GetInstallManagerInfo,
            OpenInstallWizard(0),
            ContinueWizardOnce(0),
            OpenUninstallWizard(0),
            GetInstallFolders,
        ]
    }
}

/// An expression with every integer replaced by `N` and every boolean by `B`.
fn shape(expr: &str) -> String {
    let mut out = String::with_capacity(expr.len());
    let b = expr.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) {
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push('N');
            continue;
        }
        let word_start = i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        let mut matched = false;
        for w in ["true", "false"] {
            let end = i + w.len();
            if word_start && expr[i..].starts_with(w) && (end == b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_')) {
                out.push('B');
                i = end;
                matched = true;
                break;
            }
        }
        if !matched {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

/// The template shapes every expression must match.
pub fn templates() -> Vec<String> {
    Call::samples().iter().map(|c| shape(&c.expression())).collect()
}

/// Is `expr` one of the fixed templates with only integers/booleans filled in?
pub fn is_template(expr: &str) -> bool {
    let s = shape(expr);
    templates().contains(&s)
}

/// A download in Steam's list (`RegisterForDownloadItems`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BridgeItem {
    pub appid: u32,
    pub active: bool,
    pub paused: bool,
    pub completed: bool,
    pub queue_index: i64,
    pub downloaded: u64,
    pub total: u64,
    pub buildid: u64,
    pub target_buildid: u64,
    pub update_error: String,
}

/// Steam's overview of the running download (`RegisterForDownloadOverview`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overview {
    pub appid: u32,
    pub state: String,
    pub paused: bool,
    pub downloaded: u64,
    pub to_download: u64,
    pub bytes_per_sec: u64,
    pub seconds_left: u64,
    pub is_install: bool,
}

fn num(v: &Value, k: &str) -> u64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)).unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}
fn flag(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

pub fn parse_items(items: &Value) -> Vec<BridgeItem> {
    items
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|it| {
            let appid = num(it, "appid") as u32;
            (appid != 0).then(|| BridgeItem {
                appid,
                active: flag(it, "active"),
                paused: flag(it, "paused"),
                completed: flag(it, "completed"),
                queue_index: it.get("queue_index").and_then(Value::as_i64).unwrap_or(-1),
                downloaded: num(it, "downloaded_bytes"),
                total: num(it, "total_bytes"),
                buildid: num(it, "buildid"),
                target_buildid: num(it, "target_buildid"),
                update_error: it.get("update_error").and_then(Value::as_str).unwrap_or_default().to_string(),
            })
        })
        .collect()
}

pub fn parse_overview(o: &Value) -> Overview {
    Overview {
        appid: num(o, "update_appid") as u32,
        state: o.get("update_state").and_then(Value::as_str).unwrap_or("None").to_string(),
        paused: flag(o, "paused"),
        downloaded: num(o, "update_bytes_downloaded"),
        to_download: num(o, "update_bytes_to_download"),
        bytes_per_sec: num(o, "update_network_bytes_per_second"),
        seconds_left: num(o, "update_seconds_remaining"),
        is_install: flag(o, "update_is_install"),
    }
}

/// A Steam library folder as Steam numbers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub index: u32,
    pub path: Option<String>,
    pub is_default: bool,
}

pub fn parse_folders(v: &Value) -> Vec<Folder> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|f| Folder {
            index: num(f, "nFolderIndex") as u32,
            path: f.get("strFolderPath").and_then(Value::as_str).map(str::to_string),
            is_default: flag(f, "bIsDefaultFolder"),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub enum BridgeEvent {
    Connected,
    Disconnected,
    Items { downloading: bool, items: Vec<BridgeItem> },
    Overview(Overview),
}

fn is_loopback_host(h: &str) -> bool {
    matches!(h, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// Find Steam's `SharedJSContext` page. Returns its websocket URL (never logged) or why the bridge is unavailable.
pub async fn discover(http: &reqwest::Client, port: u16) -> Result<String> {
    let base = format!("http://127.0.0.1:{port}");
    let unavailable = |why: &str| Error::Steam(format!("bridge unavailable: {why}"));
    let version: Value = http
        .get(format!("{base}/json/version"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map_err(|_| unavailable("nothing on the port"))?
        .json()
        .await
        .map_err(|_| unavailable("not DevTools"))?;
    let browser = version.get("Browser").and_then(Value::as_str).unwrap_or_default();
    if !browser.contains("Chrome") {
        return Err(unavailable("not Chrome"));
    }
    let targets: Value =
        http.get(format!("{base}/json")).timeout(Duration::from_secs(3)).send().await.map_err(|_| unavailable("no target list"))?.json().await.map_err(|_| unavailable("bad target list"))?;
    for t in targets.as_array().into_iter().flatten() {
        if t.get("title").and_then(Value::as_str) != Some("SharedJSContext") {
            continue;
        }
        let page_ok = t.get("url").and_then(Value::as_str).and_then(|u| url::Url::parse(u).ok()).is_some_and(|u| u.host_str() == Some("steamloopback.host"));
        if !page_ok {
            continue;
        }
        let Some(ws) = t.get("webSocketDebuggerUrl").and_then(Value::as_str) else { continue };
        let Ok(u) = url::Url::parse(ws) else { continue };
        if u.scheme() == "ws" && u.host_str().is_some_and(is_loopback_host) {
            return Ok(ws.to_string());
        }
    }
    Err(unavailable("no Steam page"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    Unavailable,
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

struct Conn {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
}

struct Shared {
    conn: Mutex<Option<Conn>>,
    next_id: AtomicU64,
    features: Mutex<HashMap<Ns, HashSet<String>>>,
    items: Mutex<Vec<BridgeItem>>,
    status: watch::Sender<Status>,
}

/// The bridge: keeps a connection to Steam (reconnecting with backoff while it restarts) and forwards Steam's
/// download events.
#[derive(Clone)]
pub struct SteamBridge {
    shared: Arc<Shared>,
    stop: Arc<tokio::sync::Notify>,
}

impl SteamBridge {
    pub fn spawn(port: u16, events: mpsc::UnboundedSender<BridgeEvent>) -> SteamBridge {
        let (status, _) = watch::channel(Status::Connecting);
        let shared = Arc::new(Shared { conn: Mutex::new(None), next_id: AtomicU64::new(1), features: Mutex::new(HashMap::new()), items: Mutex::new(Vec::new()), status });
        let stop = Arc::new(tokio::sync::Notify::new());
        let me = SteamBridge { shared, stop };
        let run = me.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = run.stop.notified() => {}
                _ = run.supervise(port, events) => {}
            }
            *run.shared.conn.lock().unwrap() = None;
            let _ = run.shared.status.send_replace(Status::Unavailable);
        });
        me
    }

    pub fn stop(&self) {
        self.stop.notify_waiters();
        self.stop.notify_one();
    }

    pub fn status(&self) -> Status {
        *self.shared.status.borrow()
    }

    pub fn status_watch(&self) -> watch::Receiver<Status> {
        self.shared.status.subscribe()
    }

    pub fn items(&self) -> Vec<BridgeItem> {
        self.shared.items.lock().unwrap().clone()
    }

    /// Every function name this Steam has, per namespace (sorted).
    pub fn features(&self) -> Vec<(Ns, Vec<String>)> {
        let f = self.shared.features.lock().unwrap();
        [Ns::Downloads, Ns::Installs, Ns::InstallFolder]
            .into_iter()
            .map(|ns| {
                let mut v: Vec<String> = f.get(&ns).map(|s| s.iter().cloned().collect()).unwrap_or_default();
                v.sort();
                (ns, v)
            })
            .collect()
    }

    /// Whether this Steam has the function a call needs.
    pub fn has(&self, c: Call) -> bool {
        match c.needs() {
            None => true,
            Some((ns, f)) => self.shared.features.lock().unwrap().get(&ns).is_some_and(|s| s.contains(f)),
        }
    }

    async fn supervise(&self, port: u16, events: mpsc::UnboundedSender<BridgeEvent>) {
        let http = reqwest::Client::builder().no_proxy().build().unwrap_or_default();
        let mut backoff = Duration::from_secs(1);
        loop {
            match discover(&http, port).await {
                Ok(ws) => {
                    backoff = Duration::from_secs(1);
                    if let Err(e) = self.session(&ws, &events).await {
                        tracing::debug!("steam bridge: {e}");
                    }
                    let _ = events.send(BridgeEvent::Disconnected);
                }
                Err(e) => tracing::debug!("steam bridge: {e}"),
            }
            *self.shared.conn.lock().unwrap() = None;
            let _ = self.shared.status.send_replace(Status::Unavailable);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    /// One connection, until it closes.
    async fn session(&self, ws_url: &str, events: &mpsc::UnboundedSender<BridgeEvent>) -> Result<()> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url).await.map_err(|_| Error::Steam("could not open the Steam page".into()))?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        *self.shared.conn.lock().unwrap() = Some(Conn { out: out_tx, pending: pending.clone() });
        let writer = tokio::spawn(async move {
            while let Some(m) = out_rx.recv().await {
                if sink.send(Message::Text(m.into())).await.is_err() {
                    break;
                }
            }
        });
        let reader_events = events.clone();
        let shared = self.shared.clone();
        let reader_pending = pending.clone();
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(t) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(response_value(&v));
                    }
                    continue;
                }
                if v.get("method").and_then(Value::as_str) == Some("Runtime.bindingCalled") && v.pointer("/params/name").and_then(Value::as_str) == Some(BINDING) {
                    let payload = v.pointer("/params/payload").and_then(Value::as_str).unwrap_or("{}");
                    let Ok(p) = serde_json::from_str::<Value>(payload) else { continue };
                    match p.get("kind").and_then(Value::as_str) {
                        Some("items") => {
                            let items = parse_items(p.get("items").unwrap_or(&Value::Null));
                            *shared.items.lock().unwrap() = items.clone();
                            let _ = reader_events.send(BridgeEvent::Items { downloading: flag(&p, "d"), items });
                        }
                        Some("overview") => {
                            let _ = reader_events.send(BridgeEvent::Overview(parse_overview(p.get("o").unwrap_or(&Value::Null))));
                        }
                        _ => {}
                    }
                }
            }
        });

        let setup = async {
            self.raw("Runtime.enable", json!({})).await?;
            self.raw("Runtime.addBinding", json!({ "name": BINDING })).await?;
            let mut features = HashMap::new();
            for ns in [Ns::Downloads, Ns::Installs, Ns::InstallFolder] {
                let keys = self.call(Call::Keys(ns)).await.unwrap_or(Value::Null);
                let set: HashSet<String> = keys.as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                features.insert(ns, set);
            }
            *self.shared.features.lock().unwrap() = features;
            for c in [Call::RegisterDownloadItems, Call::RegisterDownloadOverview] {
                if self.has(c) {
                    self.call(c).await?;
                }
            }
            Ok::<_, Error>(())
        };
        let result = match setup.await {
            Ok(()) => {
                let _ = self.shared.status.send_replace(Status::Connected);
                let _ = events.send(BridgeEvent::Connected);
                let _ = reader.await;
                Ok(())
            }
            Err(e) => {
                reader.abort();
                Err(e)
            }
        };
        writer.abort();
        *self.shared.conn.lock().unwrap() = None;
        for (_, tx) in pending.lock().unwrap().drain() {
            let _ = tx.send(Err(Error::Steam("disconnected".into())));
        }
        result
    }

    async fn raw(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let guard = self.shared.conn.lock().unwrap();
            let conn = guard.as_ref().ok_or_else(|| Error::Steam("not connected".into()))?;
            conn.pending.lock().unwrap().insert(id, tx);
            conn.out.send(json!({ "id": id, "method": method, "params": params }).to_string()).map_err(|_| Error::Steam("not connected".into()))?;
        }
        tokio::time::timeout(Duration::from_secs(10), rx).await.map_err(|_| Error::Steam("timed out".into()))?.map_err(|_| Error::Steam("disconnected".into()))?
    }

    /// Run one of the fixed calls. Refused when this Steam lacks the function.
    pub async fn call(&self, c: Call) -> Result<Value> {
        if !self.has(c) {
            return Err(Error::Steam(format!("this Steam has no {:?}", c.needs())));
        }
        let expr = c.expression();
        // Defence in depth: only template shapes ever leave grapple.
        if !is_template(&expr) {
            return Err(Error::Steam("refused an expression that is not a template".into()));
        }
        let v = self.raw("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true })).await?;
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            return Err(Error::Steam(err.chars().take(200).collect()));
        }
        Ok(v)
    }

    /// Steam's folder index for a library path: `GetInstallFolders()` when it names paths, else `libraryfolders.vdf`
    /// keys (`folders_vdf`, path → key); unsure → Steam's default folder.
    pub async fn folder_index(&self, path: &std::path::Path, folders_vdf: &[(u32, std::path::PathBuf)]) -> Option<u32> {
        let listed = self.call(Call::GetInstallFolders).await.map(|v| parse_folders(&v)).unwrap_or_default();
        if let Some(f) = listed.iter().find(|f| f.path.as_deref().is_some_and(|p| std::path::Path::new(p) == path)) {
            return Some(f.index);
        }
        if let Some((i, _)) = folders_vdf.iter().find(|(_, p)| p == path)
            && (listed.is_empty() || listed.iter().any(|f| f.index == *i))
        {
            return Some(*i);
        }
        listed.iter().find(|f| f.is_default).map(|f| f.index)
    }

    /// The download size Steam's install manager reports after `SetAppList`.
    pub async fn install_size(&self, appid: u32) -> Result<Option<u64>> {
        self.call(Call::SetAppList(appid)).await?;
        let info = self.call(Call::GetInstallManagerInfo).await?;
        Ok(info
            .get("rgAppIDs")
            .and_then(Value::as_array)
            .and_then(|a| a.iter().find(|x| num(x, "nAppID") as u32 == appid))
            .map(|x| num(x, "lDiskSpaceRequiredBytes"))
            .filter(|n| *n > 0)
            .or_else(|| Some(num(&info, "nDiskSpaceRequired")).filter(|n| *n > 0)))
    }

    /// Install with no Steam window: [`INSTALL_STEPS`], then, if no download shows up, [`INSTALL_FALLBACK`].
    pub async fn install(&self, appid: u32, folder: u32, wait: Duration) -> Result<()> {
        for s in INSTALL_STEPS {
            self.call(s.call(appid, folder)).await?;
        }
        if self.wait_for(appid, wait).await {
            return Ok(());
        }
        tracing::info!("steam bridge: install of {appid} did not start; trying the wizard");
        for s in INSTALL_FALLBACK {
            self.call(s.call(appid, folder)).await?;
        }
        if self.wait_for(appid, wait).await { Ok(()) } else { Err(Error::Steam("the install did not start".into())) }
    }

    async fn wait_for(&self, appid: u32, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            if self.items().iter().any(|i| i.appid == appid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }
}

/// One step of the no-window install. Nobody could test the exact sequence outside Josh's machine: change the two
/// lists below if Steam wants something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStep {
    SetAppList,
    SetInstallFolder,
    ContinueInstall,
    OpenWizard,
    ContinueWizardOnce,
}

impl InstallStep {
    fn call(self, appid: u32, folder: u32) -> Call {
        match self {
            InstallStep::SetAppList => Call::SetAppList(appid),
            InstallStep::SetInstallFolder => Call::SetInstallFolder(folder),
            InstallStep::ContinueInstall => Call::ContinueInstall,
            InstallStep::OpenWizard => Call::OpenInstallWizard(appid),
            InstallStep::ContinueWizardOnce => Call::ContinueWizardOnce(folder),
        }
    }
}

pub const INSTALL_STEPS: &[InstallStep] = &[InstallStep::SetAppList, InstallStep::SetInstallFolder, InstallStep::ContinueInstall];
/// The one-shot wizard callback is registered before the wizard opens, so it cannot miss it.
pub const INSTALL_FALLBACK: &[InstallStep] = &[InstallStep::ContinueWizardOnce, InstallStep::OpenWizard];

fn response_value(v: &Value) -> Result<Value> {
    if let Some(e) = v.get("error") {
        return Err(Error::Steam(e.get("message").and_then(Value::as_str).unwrap_or("error").chars().take(200).collect()));
    }
    if let Some(ex) = v.pointer("/result/exceptionDetails") {
        return Err(Error::Steam(ex.get("text").and_then(Value::as_str).unwrap_or("exception").chars().take(200).collect()));
    }
    Ok(v.pointer("/result/result/value").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expressions_are_templates_with_numbers_only() {
        assert!(is_template(&Call::SetQueueIndex(252490, 0).expression()));
        assert!(is_template(&Call::EnableAllDownloads(false).expression()));
        assert!(is_template(&Call::Keys(Ns::InstallFolder).expression()));
        assert!(!is_template("SteamClient.Downloads.PauseAppUpdate(\"x\")"));
        assert!(!is_template(&Call::PauseAppUpdate(1).expression().replace("PauseAppUpdate(1)", "PauseAppUpdate(1); fetch('x')")));
        assert_eq!(shape("a1(12, true) x_2 false3"), "a1(N, B) x_2 false3");
        assert_eq!(
            Call::SetAppList(5).expression(),
            "(async () => { try { return SteamClient.Installs.SetAppList([5]); } catch (e) { return ({error: String(e)}); } })()"
        );
    }

    #[test]
    fn item_and_overview_mapping() {
        let items = json!([{"appid": 252490, "active": true, "paused": false, "completed": false, "queue_index": 0, "downloaded_bytes": 100, "total_bytes": 400, "buildid": 1, "target_buildid": 2, "update_error": ""},
                           {"appid": 0}, {"appid": "x"}]);
        let p = parse_items(&items);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].appid, p[0].active, p[0].downloaded, p[0].total, p[0].queue_index), (252490, true, 100, 400, 0));
        let o = parse_overview(&json!({"update_appid": 252490, "update_state": "Updating", "paused": false, "update_bytes_downloaded": 1.5e9, "update_bytes_to_download": 3e9,
            "update_network_bytes_per_second": 4.28e7, "update_seconds_remaining": 540, "update_is_install": true}));
        assert_eq!((o.appid, o.state.as_str(), o.downloaded, o.bytes_per_sec, o.seconds_left, o.is_install), (252490, "Updating", 1_500_000_000, 42_800_000, 540, true));
    }
}
