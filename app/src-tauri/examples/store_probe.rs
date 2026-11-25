//! Store window probe: drives the real `src/store_window.rs` against local stand-ins of Steam, Epic, an off-list
//! site and Epic's OAuth service. Never talks to Steam or Epic. Run it with throwaway XDG dirs, e.g.:
//!
//! ```sh
//! CARGO_TARGET_DIR=target/app cargo run -p grapple --example store_probe --features store-probe -- \
//!     --dir /tmp/probe --phase 1          # then --phase 2 (restart: cookies must persist)
//! ```
//!
//! `--dir` holds the store-web data dir, a fake legendary user.json, the system-browser stub's log and
//! `report.json` (what each check saw). The probe exits by itself.

#[path = "../src/store_window.rs"]
#[allow(dead_code)]
mod store_window;

use javascriptcore::ValueExt;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder};
use webkit2gtk::WebViewExt;

const MOCK_CODE: &str = "MOCK-EXCHANGE-CODE-7f3a9c";
const MOCK_ACCESS_OLD: &str = "MOCK-ACCESS-OLD-1b2c";
const MOCK_REFRESH_OLD: &str = "MOCK-REFRESH-OLD-3d4e";
const MOCK_ACCESS_NEW: &str = "MOCK-ACCESS-NEW-5f6a";
const MOCK_REFRESH_NEW: &str = "MOCK-REFRESH-NEW-7b8c";

// ------------------------------------------------------------------------------------------------ tiny HTTP

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn html(body: &str) -> Resp {
    Resp { status: 200, headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())], body: body.into() }
}

fn redirect(to: &str) -> Resp {
    Resp { status: 302, headers: vec![("Location".into(), to.into())], body: String::new() }
}

type Handler = Arc<dyn Fn(&Req, &Server) -> Resp + Send + Sync>;

#[derive(Clone)]
struct Server {
    origin: String,
    seen: Arc<Mutex<Vec<Req>>>,
}

impl Server {
    fn start(handler: impl Fn(&Req, &Server) -> Resp + Send + Sync + 'static) -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Server { origin: format!("http://127.0.0.1:{}", l.local_addr().unwrap().port()), seen: Default::default() };
        let h: Handler = Arc::new(handler);
        let s2 = s.clone();
        std::thread::spawn(move || {
            for c in l.incoming().flatten() {
                let (h, s) = (h.clone(), s2.clone());
                std::thread::spawn(move || serve(c, &h, &s));
            }
        });
        s
    }
    fn seen(&self) -> Vec<Req> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(c: TcpStream, h: &Handler, s: &Server) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("").to_string();
    let path = it.next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).is_err() || l.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0usize);
    let mut body = vec![0; len];
    let _ = r.read_exact(&mut body);
    let req = Req { method, path, headers, body: String::from_utf8_lossy(&body).into() };
    s.seen.lock().unwrap().push(req.clone());
    let resp = h(&req, s);
    let reason = match resp.status {
        200 => "OK",
        302 => "Found",
        _ => "X",
    };
    let mut out = format!("HTTP/1.1 {} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n", resp.status, resp.body.len());
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(&resp.body);
    let mut c = c;
    let _ = c.write_all(out.as_bytes());
}

fn query(path: &str) -> HashMap<String, String> {
    url::Url::parse(&format!("http://x{path}")).unwrap().query_pairs().into_owned().collect()
}

// ------------------------------------------------------------------------------------------------ pages

/// Tries every way into grapple a page could know of, reports to `/report?who=<who>`.
const IPC_CHECK: &str = r#"<!doctype html><title>IPC check</title><script>
(async () => {
  const who = new URLSearchParams(location.search).get('who') || 'store';
  const r = {};
  r.tauri = typeof window.__TAURI__;
  r.internals = typeof window.__TAURI_INTERNALS__;
  r.ipc = typeof window.ipc;
  r.webkitHandler = typeof (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc);
  r.isTauri = typeof window.isTauri;
  const tryFetch = async (u, opt) => { try { const x = await fetch(u, opt); return 'ok ' + x.status + ' ' + (await x.text()).slice(0, 60); } catch (e) { return 'error ' + e; } };
  r.ipcScheme = await tryFetch('ipc://localhost/secret_cmd', { method: 'POST', body: '{}', headers: { 'Content-Type': 'application/json' } });
  r.grappleScheme = await tryFetch('grapple://art/probe');
  r.tauriScheme = await tryFetch('tauri://localhost/');
  if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
    r.invoke = await window.__TAURI_INTERNALS__.invoke('secret_cmd').then(v => 'ok ' + v, e => 'rejected ' + e);
    r.invokePlugin = await window.__TAURI_INTERNALS__.invoke('plugin:event|listen', { event: 'x', target: { kind: 'Any' }, handler: 1 }).then(v => 'ok ' + v, e => 'rejected ' + e);
  }
  if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc) {
    try { window.webkit.messageHandlers.ipc.postMessage(JSON.stringify({ cmd: 'secret_cmd', callback: 1, error: 2, payload: {} })); r.rawPost = 'sent'; } catch (e) { r.rawPost = 'error ' + e; }
  }
  await fetch('/report?who=' + who, { method: 'POST', body: JSON.stringify(r) });
  document.title = 'IPC check done';
})();
</script>"#;

fn steam_app_page() -> String {
    // Shaped like Steam's app page: a bundle box first (must be skipped), then the game's own box.
    let filler = "<p>description</p>".repeat(200);
    format!(
        r#"<!doctype html><title>Probe Game on Steam</title><body>{filler}
<div id="game_area_purchase" class="game_area_purchase">
 <div class="game_area_purchase_game_wrapper"><div class="game_area_purchase_game_dropdown_subscription game_area_purchase_game" id="bundle">
  <form name="add_bundle_to_cart_1"></form><a class="btn_green_steamui" id="btn_bundle" href="javascript:void(0)">Add bundle</a></div></div>
 <div class="game_area_purchase_game_wrapper"><div class="game_area_purchase_game" id="game_area_purchase_section_add_to_cart_570">
  <form name="add_to_cart_570" action="/cart/" method="POST"><input type="hidden" name="sessionid" value="PAGE-SESSIONID"><input type="hidden" name="subid" value="570"></form>
  <h2>Buy Probe Game</h2><div class="btn_addtocart"><a class="btn_green_steamui" href="javascript:addToCart(570);" id="btn_add_to_cart_570">Add to Cart</a></div></div></div>
</div>{filler}</body>"#
    )
}

// ------------------------------------------------------------------------------------------------ logging

/// Every tracing event at every level to stderr (grapple itself installs no subscriber; this shows everything
/// grapple's code would log).
struct StderrLog;
impl tracing::Subscriber for StderrLog {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, e: &tracing::Event<'_>) {
        struct V(String);
        impl tracing::field::Visit for V {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                self.0.push_str(&format!(" {}={:?}", f.name(), v));
            }
        }
        let mut v = V(String::new());
        e.record(&mut v);
        eprintln!("[log {} {}]{}", e.metadata().level(), e.metadata().target(), v.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

// ------------------------------------------------------------------------------------------------ probe

struct Ctx {
    app: AppHandle,
    report: Mutex<serde_json::Map<String, serde_json::Value>>,
}

impl Ctx {
    fn put(&self, k: &str, v: impl Into<serde_json::Value>) {
        let v = v.into();
        eprintln!("[probe] {k} = {v}");
        self.report.lock().unwrap().insert(k.into(), v);
    }

    /// Evaluate JS in the store window's main world. (WebKit runs API scripts as a user gesture, like a click.)
    async fn eval(&self, js: &str) -> String {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let js = js.to_string();
        self.app
            .run_on_main_thread(move || match store_window::probe_view() {
                None => {
                    let _ = tx.send("no store window".to_string());
                }
                Some(v) => v.evaluate_javascript(&js, None, None, None::<&gtk::gio::Cancellable>, move |r| {
                    let _ = tx.send(match r {
                        Ok(val) => val.to_str().to_string(),
                        Err(e) => format!("error {e}"),
                    });
                }),
            })
            .unwrap();
        rx.await.unwrap_or_default()
    }

    /// Load a URL in the store window without counting it as Josh's click.
    async fn load(&self, url: &str) {
        let url = url.to_string();
        self.app
            .run_on_main_thread(move || {
                if let Some(v) = store_window::probe_view() {
                    v.load_uri(&url);
                }
            })
            .unwrap();
    }

    async fn wait_uri(&self, pred: impl Fn(&str) -> bool, secs: u64) -> String {
        // The view's `uri` changes when a load *starts*; the page's own location only once it is shown.
        for _ in 0..secs * 10 {
            let u = self.eval("document.readyState + ' ' + location.href").await;
            if let Some(href) = u.strip_prefix("complete ")
                && pred(href)
            {
                return href.to_string();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.eval("location.href").await
    }
}

fn hypr_clients() -> Vec<serde_json::Value> {
    let out = std::process::Command::new("hyprctl").args(["clients", "-j"]).output();
    out.ok().and_then(|o| serde_json::from_slice::<Vec<serde_json::Value>>(&o.stdout).ok()).unwrap_or_default()
}

fn mine(c: &serde_json::Value) -> bool {
    c["pid"].as_u64() == Some(std::process::id() as u64)
}

async fn run(ctx: Arc<Ctx>, phase: u32, dir: PathBuf, s: Servers) {
    let sleep = |ms| tokio::time::sleep(Duration::from_millis(ms));
    let app = ctx.app.clone();

    // 1. Steam checkout: the app page, scrolled to the game's own purchase box with Add to Cart focused.
    store_window::open_checkout(&app, "steam:570", "steam").await.unwrap();
    let u = ctx.wait_uri(|u| u.contains("/app/570"), 10).await;
    ctx.put("steam.uri", u);
    sleep(800).await;
    ctx.put("steam.focused", ctx.eval("document.activeElement && document.activeElement.id").await);
    ctx.put("steam.scrolled", ctx.eval("String(window.scrollY > 1000)").await);
    ctx.put("steam.bundle_untouched", ctx.eval("String(document.getElementById('bundle').style.outline === '')").await);
    let cookies_seen: Vec<String> =
        s.steam.seen().iter().filter(|r| r.path.starts_with("/app/570")).map(|r| r.headers.get("cookie").cloned().unwrap_or_default()).collect();
    ctx.put("steam.cookie_header_on_first_load", cookies_seen.first().cloned().unwrap_or_default());

    if phase == 2 {
        // Restart check only.
        finish(&ctx, &dir).await;
        return;
    }

    // 2. Window identity (Hyprland).
    sleep(300).await;
    let wins: Vec<_> = hypr_clients().into_iter().filter(mine).collect();
    ctx.put("hypr.windows", serde_json::Value::Array(wins.iter().map(|c| serde_json::json!({"class": c["class"], "title": c["title"], "workspace": c["workspace"]["name"]})).collect()));

    // 3. IPC from the store window's pages, plus a Tauri webview on the same page as the control.
    ctx.load(&format!("{}/ipc-check?who=store", s.steam.origin)).await;
    let _ = WebviewWindowBuilder::new(&app, "control", WebviewUrl::External(format!("{}/ipc-check?who=control", s.steam.origin).parse().unwrap()))
        .visible(false)
        .build();
    for _ in 0..100 {
        let r = s.steam.seen();
        if r.iter().any(|q| q.path == "/report?who=store") && r.iter().any(|q| q.path == "/report?who=control") {
            break;
        }
        sleep(100).await;
    }
    for q in s.steam.seen().iter().filter(|q| q.path.starts_with("/report")) {
        let who = query(&q.path).get("who").cloned().unwrap_or_default();
        ctx.put(&format!("ipc.{who}"), serde_json::from_str::<serde_json::Value>(&q.body).unwrap_or_default());
    }
    ctx.put("ipc.secret_cmd_calls", s.secret_calls());

    // 4. Off-list navigation from a click: not shown, sent to the system browser.
    ctx.load(&format!("{}/plain", s.steam.origin)).await;
    ctx.wait_uri(|u| u.ends_with("/plain"), 5).await;
    ctx.eval(&format!("location.href = '{}/x?from=click'; 'ok'", s.off.origin)).await;
    sleep(1500).await;
    ctx.put("offlist_click.store_uri", ctx.eval("location.href").await);
    // 5. ... through a redirect from an allowed page.
    ctx.eval(&format!("location.href = '{}/redir-off'; 'ok'", s.steam.origin)).await;
    sleep(1500).await;
    ctx.put("offlist_redirect.store_uri", ctx.eval("location.href").await);
    // 6. Off-list navigation by a page script long after any click: blocked, nothing opened.
    sleep(1200).await;
    ctx.load(&format!("{}/auto-off", s.steam.origin)).await;
    ctx.wait_uri(|u| u.ends_with("/auto-off"), 5).await;
    sleep(7000).await;
    ctx.put("offlist_script.store_uri", ctx.eval("location.href").await);
    // 7. Popups: off-list -> browser; allowed -> a related window that can close itself.
    ctx.load(&format!("{}/plain", s.steam.origin)).await;
    ctx.wait_uri(|u| u.ends_with("/plain"), 5).await;
    sleep(1100).await;
    ctx.eval(&format!("window.open('{}/x?from=popup'); 'ok'", s.off.origin)).await;
    sleep(1200).await;
    ctx.eval(&format!("window.__w = window.open('{}/popup'); 'ok'", s.steam.origin)).await;
    sleep(1500).await;
    let pop: Vec<_> = hypr_clients().into_iter().filter(mine).map(|c| c["title"].clone()).collect();
    ctx.put("popup.windows_while_open", serde_json::Value::Array(pop));
    ctx.put("popup.opener_link", ctx.eval("String(!!(window.__w && !window.__w.closed))").await);
    sleep(2500).await;
    let pop: Vec<_> = hypr_clients().into_iter().filter(mine).map(|c| c["title"].clone()).collect();
    ctx.put("popup.windows_after_close", serde_json::Value::Array(pop));
    // 8. Off-list iframe inside an allowed page loads (payment widgets, 3-D Secure, captchas).
    ctx.load(&format!("{}/frame-host", s.steam.origin)).await;
    ctx.wait_uri(|u| u.ends_with("/frame-host"), 5).await;
    sleep(1000).await;
    ctx.put("frame.offlist_iframe_loaded", ctx.eval("String(window.__frameLoaded === true)").await);
    ctx.put("frame.store_uri", ctx.eval("location.href").await);
    // 9. Downloads refused.
    ctx.eval(&format!("location.href = '{}/download'; 'ok'", s.steam.origin)).await;
    sleep(1500).await;
    let mut a = ctx.eval(&format!("var a = document.createElement('a'); a.href = '{}/download2'; a.download = 'x.bin'; document.body.appendChild(a); a.click(); 'ok'", s.steam.origin)).await;
    sleep(1500).await;
    a.push_str(&format!(" store_uri={}", ctx.eval("location.href").await));
    ctx.put("download.attempts", a);
    // 10. Permission requests denied.
    ctx.put("permission.notification", ctx.eval("Notification.requestPermission().then(p => document.title = 'perm ' + p); 'asked'").await);
    sleep(800).await;
    ctx.put("permission.notification_result", ctx.eval("document.title").await);

    // 11. Epic: exchange sign-in (expired token: refresh, rotate, write back), then the purchase page.
    sleep(1100).await;
    store_window::open_checkout(&app, "epic:probens:probeoffer", "epic").await.unwrap();
    let u = ctx.wait_uri(|u| u.contains("/purchase"), 10).await;
    ctx.put("epic.final_uri", u);
    let ex: Vec<_> = s.epic.seen().iter().filter(|r| r.path.starts_with("/id/exchange")).map(|r| query(&r.path)).collect();
    ctx.put("epic.exchange_requests", serde_json::to_value(&ex).unwrap());
    let oauth: Vec<_> = s.oauth.seen().iter().map(|r| format!("{} {} auth={}", r.method, r.path, r.headers.get("authorization").map(|a| a.split(' ').next().unwrap_or("")).unwrap_or(""))).collect();
    ctx.put("epic.oauth_calls", serde_json::to_value(oauth).unwrap());
    let uj: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("legendary/user.json")).unwrap()).unwrap();
    ctx.put("epic.user_json_refresh_rotated", uj["refresh_token"].as_str() == Some(MOCK_REFRESH_NEW));
    ctx.put("epic.user_json_kept_other_fields", uj["keep_me"] == serde_json::json!([1, 2, 3]));
    sleep(500).await;
    let wins: Vec<_> = hypr_clients().into_iter().filter(mine).map(|c| serde_json::json!({"class": c["class"], "title": c["title"], "workspace": c["workspace"]["name"]})).collect();
    ctx.put("hypr.windows_epic", serde_json::Value::Array(wins));
    // 12. Buy from a store page only (cross-store listing): an allowed Epic page is signed in the same way; a
    //     price source's redirect link goes to the browser.
    sleep(1100).await;
    let before = s.epic.seen().len();
    store_window::open_target(&app, "epic", &format!("{}/p/probe-game", s.epic.origin)).await.unwrap();
    ctx.put("target.final_uri", ctx.wait_uri(|u| u.ends_with("/p/probe-game"), 10).await);
    let ex: Vec<_> = s.epic.seen()[before..].iter().filter(|r| r.path.starts_with("/id/exchange")).map(|r| query(&r.path)).collect();
    ctx.put("target.exchange_requests", serde_json::to_value(&ex).unwrap());
    store_window::open_target(&app, "epic", "https://itad.link/probe-redirect/").await.unwrap();
    ctx.put("target.offlist_result", ctx.eval("location.href").await);
    // 13. Epic signed out (no user.json): straight to the purchase page.
    std::fs::remove_file(dir.join("legendary/user.json")).unwrap();
    let before = s.epic.seen().len();
    store_window::open_checkout(&app, "epic:probens:probeoffer2", "epic").await.unwrap();
    ctx.wait_uri(|u| u.contains("probeoffer2"), 10).await;
    let after: Vec<String> = s.epic.seen()[before..].iter().map(|r| r.path.clone()).collect();
    ctx.put("epic.signed_out_requests", serde_json::to_value(after).unwrap());
    // 14. Bad keys.
    ctx.put("keys.bad", format!("{:?}", store_window::open_checkout(&app, "epic:a:b&x=1", "epic").await));

    finish(&ctx, &dir).await;
}

async fn finish(ctx: &Ctx, dir: &Path) {
    let browser = std::fs::read_to_string(dir.join("browser.log")).unwrap_or_default();
    ctx.put("browser_stub.opened", serde_json::to_value(browser.lines().collect::<Vec<_>>()).unwrap());
    let report = serde_json::Value::Object(ctx.report.lock().unwrap().clone());
    std::fs::write(dir.join(format!("report-{}.json", std::env::var("PROBE_PHASE").unwrap_or_default())), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    eprintln!("[probe] done");
    ctx.app.exit(0);
}

struct Servers {
    steam: Server,
    epic: Server,
    off: Server,
    oauth: Server,
    secret_calls: Arc<Mutex<u32>>,
}

impl Servers {
    fn secret_calls(&self) -> u32 {
        *self.secret_calls.lock().unwrap()
    }
}

fn main() {
    let _ = tracing::subscriber::set_global_default(StderrLog);
    let args: Vec<String> = std::env::args().collect();
    let arg = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let dir = PathBuf::from(arg("--dir").expect("--dir"));
    let phase: u32 = arg("--phase").unwrap_or("1".into()).parse().unwrap();
    // SAFETY: before any thread.
    unsafe { std::env::set_var("PROBE_PHASE", phase.to_string()) };
    std::fs::create_dir_all(dir.join("legendary")).unwrap();

    let off = Server::start(|r, _| match r.path.split('?').next().unwrap() {
        "/frame" => html("<!doctype html><script>parent.postMessage('frame-loaded', '*')</script>"),
        _ => html("<!doctype html><title>OFF-LIST PAGE</title>off-list"),
    });
    let off_origin = off.origin.clone();
    let steam = Server::start(move |r, _| {
        let p = r.path.split('?').next().unwrap();
        match p {
            "/app/570/" => {
                let mut resp = html(&steam_app_page());
                if !r.headers.contains_key("cookie") {
                    resp.headers.push(("Set-Cookie".into(), "probe=persist; Max-Age=86400; Path=/".into()));
                }
                resp
            }
            "/ipc-check" => html(IPC_CHECK),
            "/report" => html("ok"),
            "/plain" => html("<!doctype html><title>Plain stand-in</title>plain"),
            "/redir-off" => redirect(&format!("{off_origin}/x?from=redirect")),
            "/auto-off" => html(&format!("<!doctype html><title>Auto</title><script>setTimeout(() => location.href = '{off_origin}/x?from=script', 6000)</script>")),
            "/popup" => html("<!doctype html><title>Popup stand-in</title><script>setTimeout(() => window.close(), 2000)</script>"),
            "/frame-host" => html(&format!("<!doctype html><title>Frame host</title><script>addEventListener('message', e => {{ if (e.data === 'frame-loaded') window.__frameLoaded = true }})</script><iframe src='{off_origin}/frame'></iframe>")),
            "/download" | "/download2" => Resp {
                status: 200,
                headers: vec![("Content-Type".into(), "application/octet-stream".into()), ("Content-Disposition".into(), "attachment; filename=probe.bin".into())],
                body: "BINARY".into(),
            },
            _ => html("<!doctype html><title>404</title>"),
        }
    });
    let epic = Server::start(|r, s| {
        let p = r.path.split('?').next().unwrap();
        match p {
            "/id/exchange" => {
                let q = query(&r.path);
                // Epic would set its session cookie here and continue to redirectUrl.
                let to = q.get("redirectUrl").cloned().unwrap_or_else(|| format!("{}/", s.origin));
                let mut resp = redirect(&to);
                resp.headers.push(("Set-Cookie".into(), "EPIC_SESSION=stand-in; Path=/".into()));
                resp
            }
            "/purchase" => html("<!doctype html><title>Epic purchase stand-in</title>purchase"),
            "/p/probe-game" => html("<!doctype html><title>Epic product stand-in</title>product"),
            _ => html("<!doctype html><title>404</title>"),
        }
    });
    let oauth = Server::start(|r, _| {
        let auth = r.headers.get("authorization").cloned().unwrap_or_default();
        match r.path.as_str() {
            "/account/api/oauth/token" if r.body.contains(&format!("refresh_token={MOCK_REFRESH_OLD}")) => Resp {
                status: 200,
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: format!(r#"{{"access_token":"{MOCK_ACCESS_NEW}","refresh_token":"{MOCK_REFRESH_NEW}","expires_at":"2099-01-01T00:00:00.000Z","account_id":"acc1","displayName":"Probe"}}"#),
            },
            "/account/api/oauth/exchange" if auth == format!("Bearer {MOCK_ACCESS_NEW}") => Resp {
                status: 200,
                headers: vec![("Content-Type".into(), "application/json".into())],
                body: format!(r#"{{"code":"{MOCK_CODE}","expiresInSeconds":300}}"#),
            },
            _ => Resp { status: 401, headers: vec![], body: r#"{"errorCode":"errors.probe.denied"}"#.into() },
        }
    });
    std::fs::write(
        dir.join("legendary/user.json"),
        format!(r#"{{"access_token":"{MOCK_ACCESS_OLD}","refresh_token":"{MOCK_REFRESH_OLD}","expires_at":"2000-01-01T00:00:00.000Z","account_id":"acc1","displayName":"Probe","keep_me":[1,2,3]}}"#),
    )
    .unwrap();

    let browser_log = dir.join("browser.log");
    store_window::configure(store_window::Config {
        data_dir: dir.join("store-web"),
        allow: store_window::AllowList::production().with_test_origin(&steam.origin).with_test_origin(&epic.origin),
        origins: store_window::Origins { steam_store: steam.origin.clone(), epic_id: epic.origin.clone(), epic_store: epic.origin.clone() },
        external: Arc::new(move |u: &url::Url| {
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&browser_log).unwrap();
            let _ = writeln!(f, "{u}");
        }),
        epic_api: grapple_launch::epic_auth::EpicApi {
            oauth_base: oauth.origin.clone(),
            ecommerce_base: oauth.origin.clone(),
            ..Default::default()
        },
        epic_user_json: dir.join("legendary/user.json"),
    });

    let secret_calls = Arc::new(Mutex::new(0u32));
    let servers = Servers { steam, epic, off, oauth, secret_calls: secret_calls.clone() };
    let mut context = tauri::generate_context!();
    context.config_mut().app.windows.clear();
    let sc = secret_calls.clone();
    tauri::Builder::default()
        // grapple's art protocol stand-in and an app command: what a Tauri webview's page could reach.
        .register_uri_scheme_protocol("grapple", |_ctx, _req| {
            tauri::http::Response::builder().header("Access-Control-Allow-Origin", "*").body(b"PROBE-SECRET-ART".to_vec()).unwrap()
        })
        .invoke_handler(move |invoke| {
            if invoke.message.command() == "secret_cmd" {
                *sc.lock().unwrap() += 1;
                invoke.resolver.resolve("PROBE-SECRET-DATA");
                return true;
            }
            false
        })
        .setup(move |app| {
            let ctx = Arc::new(Ctx { app: app.handle().clone(), report: Default::default() });
            let dir = dir.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let watchdog = ctx.app.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    eprintln!("[probe] watchdog: giving up");
                    watchdog.exit(2);
                });
                run(ctx, phase, dir, servers).await;
            });
            Ok(())
        })
        .run(context)
        .expect("probe");
}
