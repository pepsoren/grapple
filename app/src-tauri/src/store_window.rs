//! The store window: Steam's and Epic's own checkout and sign-in pages, isolated from grapple.
//! Binding spec: docs/STORE_PLAN.md "Checkout". How to test it: the `store_probe` example.
//!
//! # Why this is not a Tauri webview
//! Tauri gives *every* webview it creates its IPC bootstrap (`window.__TAURI_INTERNALS__` with the per-run
//! invoke key, the `ipc` script message handler) and registers `ipc://`, `tauri://` and grapple's `grapple://`
//! on its web context. Capabilities only make the Rust side refuse calls from pages they do not cover. This
//! window is a plain WebKitGTK `WebView` in its own GTK window instead, with its own `WebContext`: no script of
//! grapple's is injected, no message handler or URI scheme is registered, no Tauri event can reach it. Its pages
//! cannot even name a way into grapple. The one exception is the Steam purchase-box script below, which runs in
//! an isolated script world, once, and only touches the DOM of that page.
//!
//! # What the window does
//! * Persistent website data (cookies, local storage) in `$XDG_DATA_HOME/grapple/store-web`, so Josh signs in to
//!   Steam once. No disk cache (the Epic sign-in URL carries a one-time code), no saved HTTP credentials,
//!   downloads cancelled, every permission request (camera, location, notifications, ...) denied, no dev tools.
//! * The main frame may only show allow-listed https hosts ([`AllowList::production`]). Anything else is
//!   cancelled before it is shown and, when Josh clicked it, opened in his browser instead (with his session
//!   environment, never grapple's). Frames inside an allowed page are not restricted: payment widgets, captchas
//!   and Epic's whole checkout live in iframes, and the page embedding them is Steam's or Epic's. WebKit cannot
//!   tell a frame's navigation from the main frame's before the request, so the main-frame check is made on the
//!   response, before anything is shown (`is_main_frame_main_resource`).
//! * Payment popups stay in the app: Steam opens PayPal (`/paypal/launchauth`), other methods
//!   (`/checkout/externallink`) and the card's 3-D Secure step (a form POST to the card issuer into a window named
//!   `_authentication`) as popups that need Josh's Steam session and `window.opener`. A popup a store page opens
//!   (to an allowed page, to `about:blank`, or by a form POST) becomes a small related window whose main frame may
//!   show any https page (3-D Secure hosts are the issuers' own and cannot be listed); its title names the host.
//!   Other popups (a plain link to another site) go to the browser.
//! * Title: "<store> · <page title>". Wayland app id (Hyprland class): `grapple-store`.
//! * Keys: Alt+Left / Alt+Right back and forward, F5 / Ctrl+R reload, Ctrl+W close.
//!
//! Secrets: the Epic exchange code is a `Secret` from grapple-launch; the URL that carries it is a [`NavUrl::Secret`]
//! whose Debug/Display are redacted. It is exposed once, at `load_uri`. Logs name hosts only, never full URLs.
//!
//! # On Windows
//! There is no WebKitGTK: the window is a WebView2 view made with wry directly (not through Tauri, so again no IPC,
//! no URI scheme, no grapple script), with its own user data folder and the same allow-list and decisions. How
//! that is guaranteed, and what differs, is in `store_window_win.rs`.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tauri::AppHandle;
use url::Url;

/// Window title prefix and Hyprland class.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const APP_ID: &str = "grapple-store";

/// The Tauri label of the store window on Windows (a plain window: no webview of Tauri's, no capability). Its close
/// button closes it for real (main.rs hides other windows instead).
#[cfg_attr(not(windows), allow(dead_code))]
pub const WINDOW_LABEL: &str = "store";

// ---------------------------------------------------------------------------------------------------------------
// Store keys and checkout URLs (pure)

/// Which checkout to open, parsed from a StoreKey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkout {
    Steam { appid: u32 },
    Epic { namespace: String, offer: String },
}

impl Checkout {
    /// `key` is the StoreKey of the listing on `store`: `steam:<appid>` for `store = "steam"`,
    /// `epic:<namespace>:<offerId>` for `store = "epic"`.
    pub fn parse(key: &str, store: &str) -> Result<Checkout, String> {
        let mut parts = key.split(':');
        let kind = parts.next().unwrap_or_default();
        if kind != store {
            return Err(format!("{key:?} is not a {store} store key"));
        }
        let rest: Vec<&str> = parts.collect();
        match (kind, rest.as_slice()) {
            ("steam", [id]) => match id.parse::<u32>() {
                Ok(appid) if appid > 0 && id.bytes().all(|b| b.is_ascii_digit()) => Ok(Checkout::Steam { appid }),
                _ => Err(format!("bad Steam app id in {key:?}")),
            },
            ("epic", [ns, offer]) if epic_id_ok(ns) && epic_id_ok(offer) => {
                Ok(Checkout::Epic { namespace: ns.to_string(), offer: offer.to_string() })
            }
            ("epic", _) => Err(format!("bad Epic key {key:?} (want epic:<namespace>:<offerId>)")),
            _ => Err(format!("unknown store {store:?}")),
        }
    }

    pub fn store_name(&self) -> &'static str {
        match self {
            Checkout::Steam { .. } => "Steam",
            Checkout::Epic { .. } => "Epic Games",
        }
    }
}

/// Epic namespaces and offer ids are hex ids or short words. Anything else could break out of the URL.
fn epic_id_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Where the stores live. Only the store probe points these at local stand-ins.
#[derive(Debug, Clone)]
pub struct Origins {
    pub steam_store: String,
    #[cfg_attr(not(feature = "real"), allow(dead_code))] // the exchange sign-in needs grapple-launch
    pub epic_id: String,
    pub epic_store: String,
}

impl Origins {
    pub fn production() -> Origins {
        Origins {
            steam_store: "https://store.steampowered.com".into(),
            epic_id: "https://www.epicgames.com".into(),
            epic_store: "https://store.epicgames.com".into(),
        }
    }

    /// The game's store page; the purchase box is on it (Steam has no URL that adds to the cart by itself).
    pub fn steam_app(&self, appid: u32) -> String {
        format!("{}/app/{appid}/", self.steam_store)
    }

    pub fn epic_purchase(&self, namespace: &str, offer: &str) -> String {
        format!("{}/purchase?offers=1-{namespace}-{offer}", self.epic_store)
    }

    /// Epic's web sign-in with an exchange code, continuing to `redirect`. The result contains the code: keep it in
    /// a [`NavUrl::Secret`].
    #[cfg_attr(not(feature = "real"), allow(dead_code))]
    pub fn epic_exchange(&self, code: &str, redirect: &str) -> String {
        let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        format!("{}/id/exchange?exchangeCode={}&redirectUrl={}", self.epic_id, enc(code), enc(redirect))
    }
}

/// A URL for the store window. The secret one carries the Epic exchange code.
pub enum NavUrl {
    Plain(String),
    #[cfg(feature = "real")]
    Secret(grapple_launch::plan::Secret),
}

impl NavUrl {
    fn expose(&self) -> &str {
        match self {
            NavUrl::Plain(s) => s,
            #[cfg(feature = "real")]
            NavUrl::Secret(s) => s.expose(),
        }
    }
}

impl std::fmt::Debug for NavUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavUrl::Plain(s) => write!(f, "{}", host_of(s)),
            #[cfg(feature = "real")]
            NavUrl::Secret(_) => f.write_str("<redacted url>"),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The allow-list (pure)

/// A host the main frame may show. `paths` empty = the whole host; else only paths starting with one of them.
#[derive(Debug, Clone)]
struct HostRule {
    host: &'static str,
    paths: &'static [&'static str],
}

const fn host(host: &'static str) -> HostRule {
    HostRule { host, paths: &[] }
}

/// Main-frame hosts. Keep this tight: a host here can be shown full-window with no address bar.
const PRODUCTION_HOSTS: &[HostRule] = &[
    // Steam: store (app pages, cart, sign-in at /login), checkout, the sign-in service, support.
    host("store.steampowered.com"),
    host("checkout.steampowered.com"),
    host("login.steampowered.com"),
    host("help.steampowered.com"),
    // steamcommunity.com only for signing in (Steam's OpenID and login pages). Its linkfilter, profiles and
    // the rest open in the browser.
    HostRule { host: "steamcommunity.com", paths: &["/login", "/openid"] },
    // Epic: the store (purchase page) and the account site (/id/ sign-in, exchange, MFA).
    host("store.epicgames.com"),
    host("www.epicgames.com"),
    // Epic's PCI purchase app (normally framed inside the store page; listed in case it is ever top-level).
    host("payment-website-pci.ol.epicgames.com"),
    // Payment processors a checkout may send the whole page to (Epic: "you will be redirected to their website").
    // Steam's payment steps are popups (see the module docs), which do not need this list.
    host("www.paypal.com"),
    host("secure.xsolla.com"),
];

#[derive(Debug, Clone)]
pub struct AllowList {
    hosts: Vec<HostRule>,
    /// Plain-http local origins for the store probe (`http://127.0.0.1:<port>`). Empty in grapple.
    test_origins: Vec<url::Origin>,
}

impl AllowList {
    pub fn production() -> AllowList {
        AllowList { hosts: PRODUCTION_HOSTS.to_vec(), test_origins: Vec::new() }
    }

    /// Test only: also allow a local stand-in origin. Refuses anything but loopback http.
    #[allow(dead_code)] // used by examples/store_probe.rs
    pub fn with_test_origin(mut self, origin: &str) -> AllowList {
        let u = Url::parse(origin).expect("test origin");
        assert!(u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1")), "test origins are loopback http only");
        self.test_origins.push(u.origin());
        self
    }

    /// May the main frame show `url`?
    pub fn allows(&self, url: &Url) -> bool {
        match url.scheme() {
            "about" => return matches!(url.path(), "blank" | "srcdoc"),
            // blob: URLs carry their creator's origin: allowed when that origin is.
            "blob" => return Url::parse(url.path()).map(|inner| inner.scheme() != "blob" && self.allows(&inner)).unwrap_or(false),
            _ => {}
        }
        if self.test_origins.contains(&url.origin()) {
            return true;
        }
        if url.scheme() != "https" || url.port().is_some() || !url.username().is_empty() || url.password().is_some() {
            return false;
        }
        let Some(h) = url.host_str() else { return false };
        self.hosts.iter().any(|r| {
            r.host == h && (r.paths.is_empty() || r.paths.iter().any(|p| path_under(url.path(), p)))
        })
    }
}

/// `/login` matches `/login`, `/login/` and `/login/home/`, not `/loginx`.
fn path_under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?'))
}

/// What the window does with a main-frame response for `url`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MainFrame {
    Show,
    /// Not shown; opened in the system browser (Josh clicked something that leads off the stores).
    Browser,
    /// Not shown, nothing opened (a page script, not Josh).
    Block,
}

/// `popup`: the view is a payment popup a store page opened; its main frame may show any https page.
pub fn main_frame_decision(allow: &AllowList, url: &str, user_initiated: bool, popup: bool) -> MainFrame {
    match Url::parse(url) {
        Ok(u) if allow.allows(&u) => MainFrame::Show,
        Ok(u) if popup && plain_https(&u) => MainFrame::Show,
        Ok(u) if user_initiated && matches!(u.scheme(), "http" | "https") => MainFrame::Browser,
        _ => MainFrame::Block,
    }
}

fn plain_https(u: &Url) -> bool {
    u.scheme() == "https" && u.host_str().is_some() && u.username().is_empty() && u.password().is_none()
}

/// What `window.open` / `target=` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    /// A related window in the app (keeps the session and `window.opener`).
    InApp,
    Browser,
    Block,
}

/// `method`: the request's HTTP method (a form POST into a named window is how Steam starts 3-D Secure).
pub fn popup_decision(allow: &AllowList, url: &str, method: &str, user_initiated: bool) -> Popup {
    if url.is_empty() || url == "about:blank" {
        return Popup::InApp; // scripts open a blank window and then point it somewhere (PayPal's SDK does)
    }
    let Ok(u) = Url::parse(url) else { return Popup::Block };
    if allow.allows(&u) || (method.eq_ignore_ascii_case("POST") && plain_https(&u)) {
        Popup::InApp
    } else if user_initiated && matches!(u.scheme(), "http" | "https") {
        Popup::Browser
    } else {
        Popup::Block
    }
}

/// The host of a URL, for logs (never the path or query: they can carry codes).
fn host_of(url: &str) -> String {
    Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "<no host>".into())
}

// ---------------------------------------------------------------------------------------------------------------
// Configuration

/// Opens a URL in the system browser.
pub type External = Arc<dyn Fn(&Url) + Send + Sync>;

pub struct Config {
    /// Cookies and site data. `$XDG_DATA_HOME/grapple/store-web`.
    pub data_dir: PathBuf,
    pub allow: AllowList,
    pub origins: Origins,
    pub external: External,
    #[cfg(feature = "real")]
    pub epic_api: grapple_launch::epic_auth::EpicApi,
    /// legendary's login (Heroic's copy). Missing = signed out: the purchase page asks Josh to sign in.
    #[cfg(feature = "real")]
    pub epic_user_json: PathBuf,
}

impl Config {
    pub fn production() -> Config {
        Config {
            data_dir: grapple_core::dirs::grapple_data().join("store-web"),
            allow: AllowList::production(),
            origins: Origins::production(),
            external: Arc::new(system_browser),
            #[cfg(feature = "real")]
            epic_api: grapple_launch::epic_auth::EpicApi::default(),
            #[cfg(feature = "real")]
            epic_user_json: grapple_launch::epic_auth::default_user_json(),
        }
    }
}

static CONFIG: OnceLock<Arc<Config>> = OnceLock::new();

/// Replace the production configuration (the store probe). Call before the first checkout.
#[allow(dead_code)] // used by examples/store_probe.rs
pub fn configure(cfg: Config) {
    let _ = CONFIG.set(Arc::new(cfg));
}

fn config() -> Arc<Config> {
    CONFIG.get_or_init(|| Arc::new(Config::production())).clone()
}

/// The default browser through the shell (`ShellExecuteW`), outside the store window.
#[cfg(windows)]
fn system_browser(url: &Url) {
    if let Err(e) = crate::system::shell_open(url.as_str()) {
        tracing::warn!("store window: could not open the browser: {e}");
    }
}

/// `xdg-open <url>` with Josh's session environment (not grapple's own variables), outside the store window.
#[cfg(not(windows))]
fn system_browser(url: &Url) {
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url.as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(feature = "real")]
    cmd.env_clear().envs(grapple_launch::env::for_child());
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => tracing::warn!("store window: could not start xdg-open: {e}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Entry points

/// Open the checkout for a store listing in the store window (creating or reusing it).
///
/// `key` is the StoreKey of the listing on `store` (`"steam"`: `steam:<appid>`, `"epic"`: `epic:<namespace>:<offerId>`).
/// Steam: the game's page, scrolled to its purchase box with "Add to Cart" focused. Epic: the purchase page,
/// signed in with a fresh exchange code from Josh's Epic login when there is one.
pub async fn open_checkout(app: &AppHandle, key: &str, store: &str) -> Result<(), String> {
    let checkout = Checkout::parse(key, store)?;
    let cfg = config();
    let nav = match &checkout {
        Checkout::Steam { appid } => Nav {
            url: NavUrl::Plain(cfg.origins.steam_app(*appid)),
            store: checkout.store_name(),
            focus_steam_app: Some(*appid),
        },
        Checkout::Epic { namespace, offer } => {
            let purchase = cfg.origins.epic_purchase(namespace, offer);
            Nav { url: epic_sign_in(&cfg, purchase).await, store: checkout.store_name(), focus_steam_app: None }
        }
    };
    tracing::info!("store window: {} checkout ({:?})", nav.store, nav.url);
    show_window(app, cfg, nav)
}

#[cfg(target_os = "linux")]
fn show_window(app: &AppHandle, cfg: Arc<Config>, nav: Nav) -> Result<(), String> {
    app.run_on_main_thread(move || gtk_side::show(cfg, nav)).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn show_window(app: &AppHandle, cfg: Arc<Config>, nav: Nav) -> Result<(), String> {
    win_side::show(app, cfg, nav)
}

/// Is `key` the StoreKey of a listing on `store` (so [`open_checkout`] can open it without asking the store service)?
pub fn key_is_for(key: &str, store: &str) -> bool {
    key.split(':').next() == Some(store)
}

/// Buy for a listing only known by its store page (e.g. a Steam game sold on Epic, from the price source): an
/// allowed store page opens in the store window (an Epic page signed in like a checkout); any other link (a price
/// source's redirect) opens in the system browser, because Josh pressed Buy.
pub async fn open_target(app: &AppHandle, store: &str, url: &str) -> Result<(), String> {
    let cfg = config();
    let u = Url::parse(url).map_err(|_| "bad store link".to_string())?;
    let name = match store {
        "steam" => "Steam",
        "epic" => "Epic Games",
        _ => return Err(format!("unknown store {store:?}")),
    };
    if !cfg.allow.allows(&u) {
        if !matches!(u.scheme(), "http" | "https") {
            return Err("bad store link".into());
        }
        tracing::info!("store window: Buy link on {} opens in the browser", host_of(url));
        (cfg.external)(&u);
        return Ok(());
    }
    let nav_url = if store == "epic" { epic_sign_in(&cfg, url.to_string()).await } else { NavUrl::Plain(url.to_string()) };
    let nav = Nav { url: nav_url, store: name, focus_steam_app: None };
    tracing::info!("store window: {name} page ({:?})", nav.url);
    show_window(app, cfg, nav)
}

/// The store window's site data (cookies, local storage): what "Sign out of store" clears. `None` when it was
/// never opened.
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub fn site_data() -> Option<PathBuf> {
    let dir = config().data_dir.clone();
    dir.is_dir().then_some(dir)
}

/// "Sign out of store" (Settings › Accounts): close the store window and delete its site data, so the stores ask
/// Josh to sign in again. His Steam app and Epic login (legendary) are not touched.
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub async fn sign_out(app: &AppHandle) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        #[cfg(target_os = "linux")]
        gtk_side::close();
        #[cfg(windows)]
        win_side::close();
        let _ = tx.send(());
    })
    .map_err(|e| e.to_string())?;
    let _ = rx.await;
    let dir = config().data_dir.clone();
    tokio::task::spawn_blocking(move || remove_site_data(&dir))
    .await
    .map_err(|e| e.to_string())?
}

/// Delete the store window's folder. WebView2's browser process lets go of its files a moment after the view closes,
/// so on Windows a busy file is waited for (up to 5 s).
fn remove_site_data(dir: &std::path::Path) -> Result<(), String> {
    let mut tries = 0;
    loop {
        match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) if cfg!(windows) && tries < 50 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
            Ok(()) => return Ok(()),
        }
    }
}

/// With Josh's Epic login: the exchange sign-in that continues to `purchase`. Without one (or when Epic says no):
/// `purchase` itself, where Epic asks him to sign in.
#[cfg(feature = "real")]
async fn epic_sign_in(cfg: &Config, purchase: String) -> NavUrl {
    use grapple_launch::epic_auth::EpicAuth;
    if !cfg.epic_user_json.exists() {
        return NavUrl::Plain(purchase);
    }
    let code = match EpicAuth::new(cfg.epic_api.clone(), cfg.epic_user_json.clone()) {
        Ok(auth) => auth.exchange_code().await,
        Err(e) => Err(e),
    };
    match code {
        Ok(code) => NavUrl::Secret(grapple_launch::plan::Secret::new(cfg.origins.epic_exchange(code.expose(), &purchase))),
        Err(e) => {
            // LaunchError's text never contains a token (epic_auth.rs).
            tracing::info!("store window: no Epic sign-in for the store ({e}); opening the purchase page signed out");
            NavUrl::Plain(purchase)
        }
    }
}

#[cfg(not(feature = "real"))]
async fn epic_sign_in(_cfg: &Config, purchase: String) -> NavUrl {
    NavUrl::Plain(purchase)
}

/// One navigation of the store window.
#[derive(Debug)]
pub struct Nav {
    pub url: NavUrl,
    pub store: &'static str,
    /// Run the purchase-box script once the page of this Steam app has loaded.
    pub focus_steam_app: Option<u32>,
}

/// Scrolls the game's own purchase box into view and focuses its "Add to Cart" (Enter adds it; Steam asks a
/// signed-out user to sign in). Runs in an isolated script world: it sees the DOM only, not the page's scripts,
/// and it reads nothing (no cookies, no form values) and sends nothing.
const STEAM_FOCUS_JS: &str = r#"(() => {
  const area = document.getElementById('game_area_purchase');
  if (!area) return 'no purchase area';
  const boxes = Array.from(area.querySelectorAll('.game_area_purchase_game'))
    .filter(b => !b.classList.contains('game_area_purchase_game_dropdown_subscription')
              && b.querySelector('form[name^="add_to_cart_"]'));
  const box = boxes[0] || area;
  box.scrollIntoView({ block: 'center' });
  box.style.outline = '2px solid #66c0f4';
  box.style.outlineOffset = '6px';
  const btn = box.querySelector('a[id^="btn_add_to_cart_"]') || box.querySelector('.btn_addtocart a');
  if (btn) btn.focus({ preventScroll: true });
  return btn ? 'focused' : 'scrolled';
})()"#;

/// `<steam_store>/app/<appid>` followed by `/`, `?`, `#` or nothing.
fn is_steam_app_page(steam_store: &str, uri: &str, appid: u32) -> bool {
    let prefix = format!("{steam_store}/app/{appid}");
    uri.strip_prefix(&prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('#'))
}

// ---------------------------------------------------------------------------------------------------------------
// The Windows side (WebView2 through wry, see the module docs there).

#[cfg(windows)]
#[path = "store_window_win.rs"]
mod win_side;

// ---------------------------------------------------------------------------------------------------------------
// The GTK side. Everything below runs on the GTK main thread.

#[cfg(target_os = "linux")]
mod gtk_side {
    use super::*;
    use gtk::prelude::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::time::{Duration, Instant};
    use webkit2gtk::{
        CacheModel, CookieManagerExt, CookiePersistentStorage, DownloadExt, LoadEvent, NavigationPolicyDecision,
        NavigationPolicyDecisionExt,
        PermissionRequestExt, PolicyDecision, PolicyDecisionExt, PolicyDecisionType, ResponsePolicyDecision,
        ResponsePolicyDecisionExt, SettingsExt, TLSErrorsPolicy, URIRequestExt, URIResponseExt, WebContext,
        WebContextExt, WebView, WebViewExt, WebsiteDataManager, WebsiteDataManagerExt,
    };

    /// A click (or key press) counts for navigations that start within this time: long enough for a few
    /// redirects, short enough that a page script cannot ride on an old click.
    const GESTURE_WINDOW: Duration = Duration::from_secs(5);
    /// At most one system-browser open per second, whatever a page does.
    const EXTERNAL_GAP: Duration = Duration::from_secs(1);

    struct Shared {
        cfg: Arc<Config>,
        settings: webkit2gtk::Settings,
        store: Cell<&'static str>,
        last_gesture: Cell<Option<Instant>>,
        last_external: Cell<Option<Instant>>,
        focus_steam_app: Cell<Option<u32>>,
    }

    struct StoreWindow {
        window: gtk::Window,
        view: WebView,
        shared: Rc<Shared>,
    }

    thread_local! {
        static WINDOW: RefCell<Option<StoreWindow>> = const { RefCell::new(None) };
    }

    /// Close the store window if it is open (its web context goes with it).
    #[cfg_attr(not(feature = "real"), allow(dead_code))]
    pub fn close() {
        let sw = WINDOW.with(|w| w.borrow_mut().take());
        if let Some(sw) = sw {
            sw.window.close();
        }
    }

    pub fn show(cfg: Arc<Config>, nav: Nav) {
        WINDOW.with(|w| {
            let mut slot = w.borrow_mut();
            if slot.is_none() {
                match create(cfg) {
                    Ok(sw) => *slot = Some(sw),
                    Err(e) => {
                        tracing::warn!("store window: {e}");
                        return;
                    }
                }
            }
            let sw = slot.as_ref().unwrap();
            sw.shared.store.set(nav.store);
            sw.shared.focus_steam_app.set(nav.focus_steam_app);
            // A load grapple asks for counts as Josh's own (he pressed Buy).
            sw.shared.last_gesture.set(Some(Instant::now()));
            sw.window.set_title(nav.store);
            sw.view.load_uri(nav.url.expose());
            drop(nav);
            sw.window.present();
        });
    }

    #[cfg(feature = "store-probe")]
    #[allow(dead_code)]
    pub fn probe_view() -> Option<WebView> {
        WINDOW.with(|w| w.borrow().as_ref().map(|sw| sw.view.clone()))
    }

    fn create(cfg: Arc<Config>) -> Result<StoreWindow, String> {
        let dir = &cfg.data_dir;
        create_private_dir(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let data = WebsiteDataManager::builder()
            .base_data_directory(dir.to_string_lossy().as_ref())
            .base_cache_directory(dir.join("cache").to_string_lossy().as_ref())
            .build();
        // Never let WebKit keep HTTP-auth passwords (it would use the keyring).
        data.set_persistent_credential_storage_enabled(false);
        data.set_tls_errors_policy(TLSErrorsPolicy::Fail);
        if let Some(cookies) = data.cookie_manager() {
            cookies.set_persistent_storage(&dir.join("cookies.sqlite").to_string_lossy(), CookiePersistentStorage::Sqlite);
        }
        let context = WebContext::builder().website_data_manager(&data).build();
        context.set_automation_allowed(false);
        // No disk cache: nothing a store page loads (one carries the Epic exchange code) is written to disk.
        context.set_cache_model(CacheModel::DocumentViewer);
        context.set_spell_checking_enabled(false);
        // Downloads are off. The response policy already refuses what WebKit cannot show; this catches
        // `<a download>` and anything else that starts one.
        context.connect_download_started(|_, download| {
            tracing::info!("store window: download refused");
            download.cancel();
        });

        let settings = webkit2gtk::Settings::new();
        settings.set_enable_developer_extras(false);
        settings.set_javascript_can_open_windows_automatically(false);
        settings.set_javascript_can_access_clipboard(false);
        settings.set_enable_write_console_messages_to_stdout(false);
        settings.set_allow_file_access_from_file_urls(false);
        settings.set_allow_universal_access_from_file_urls(false);
        settings.set_enable_media_stream(false);
        settings.set_enable_webrtc(false);

        let shared = Rc::new(Shared {
            cfg,
            settings: settings.clone(),
            store: Cell::new("Store"),
            last_gesture: Cell::new(None),
            last_external: Cell::new(None),
            focus_steam_app: Cell::new(None),
        });

        let view = WebView::builder().web_context(&context).settings(&settings).build();
        let window = toplevel(&view, 1100, 820);
        wire(&view, &window, &shared, false);
        window.connect_delete_event(|_, _| {
            // Closing drops the window and its web context; cookies are already on disk.
            WINDOW.with(|w| w.borrow_mut().take());
            gtk::glib::Propagation::Proceed
        });
        window.show_all();
        set_app_id(&window);
        Ok(StoreWindow { window, view, shared })
    }

    fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }

    fn toplevel(view: &WebView, w: i32, h: i32) -> gtk::Window {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_default_size(w, h);
        window.add(view);
        let v = view.clone();
        window.connect_key_press_event(move |win, ev| {
            use gtk::gdk::{ModifierType, keys::constants as k};
            let key = ev.keyval();
            let m = ev.state() & (ModifierType::CONTROL_MASK | ModifierType::MOD1_MASK | ModifierType::SHIFT_MASK);
            let alt = m == ModifierType::MOD1_MASK;
            let ctrl = m == ModifierType::CONTROL_MASK;
            if alt && key == k::Left {
                v.go_back();
            } else if alt && key == k::Right {
                v.go_forward();
            } else if (m.is_empty() && key == k::F5) || (ctrl && (key == k::r || key == k::R)) {
                v.reload();
            } else if ctrl && (key == k::w || key == k::W) {
                win.close();
            } else {
                return gtk::glib::Propagation::Proceed;
            }
            gtk::glib::Propagation::Stop
        });
        window
    }

    /// Hyprland's class is the Wayland app id; give this window its own so a rule can float it.
    fn set_app_id(window: &gtk::Window) {
        use gtk::glib::translate::ToGlibPtr;
        let Some(gdk_window) = window.window() else { return };
        let is_wayland = gdk_window.display().type_().name() == "GdkWaylandDisplay";
        if !is_wayland {
            return;
        }
        let id = std::ffi::CString::new(APP_ID).unwrap();
        let ptr: *mut gtk::gdk::ffi::GdkWindow = gdk_window.to_glib_none().0;
        // SAFETY: a realized GdkWindow of a Wayland display, and a NUL-terminated string GTK copies.
        unsafe { gdk_wayland_sys::gdk_wayland_window_set_application_id(ptr as *mut _, id.as_ptr()) };
    }

    /// `is_popup`: a payment popup (any https main frame, host in the title).
    fn wire(view: &WebView, window: &gtk::Window, shared: &Rc<Shared>, is_popup: bool) {
        let is_main = !is_popup;
        // Title: "<store> · <page title>" (popups: "<store> · <host> · <page title>")
        {
            let win = window.clone();
            let sh = shared.clone();
            let update = move |v: &WebView| {
                let host = v.uri().map(|u| host_of(&u)).unwrap_or_default();
                let page = v.title().map(|t| t.to_string()).filter(|t| !t.trim().is_empty());
                let title = match (is_popup, page) {
                    (true, Some(p)) => format!("{} · {host} · {p}", sh.store.get()),
                    (true, None) => format!("{} · {host}", sh.store.get()),
                    (false, Some(p)) => format!("{} · {p}", sh.store.get()),
                    (false, None) => format!("{} · {host}", sh.store.get()),
                };
                win.set_title(&title);
            };
            let u2 = update.clone();
            view.connect_title_notify(move |v| update(v));
            view.connect_uri_notify(move |v| u2(v));
        }

        {
            let sh = shared.clone();
            view.connect_decide_policy(move |v, decision, kind| decide(&sh, v, decision, kind, is_popup));
        }

        {
            let sh = shared.clone();
            view.connect_create(move |parent, action| {
                let req = action.request();
                let url = req.as_ref().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
                let method = req.as_ref().and_then(|r| r.http_method()).map(|m| m.to_string()).unwrap_or_default();
                let gesture = action.is_user_gesture();
                if gesture {
                    sh.last_gesture.set(Some(Instant::now()));
                }
                match popup_decision(&sh.cfg.allow, &url, &method, gesture) {
                    Popup::InApp => {
                        tracing::info!("store window: popup for {}", host_of(&url));
                        Some(popup(parent, &sh).upcast::<gtk::Widget>())
                    }
                    Popup::Browser => {
                        external(&sh, &url);
                        None
                    }
                    Popup::Block => {
                        tracing::info!("store window: popup to {} blocked", host_of(&url));
                        None
                    }
                }
            });
        }

        view.connect_permission_request(|_, request| {
            request.deny();
            true
        });

        if is_main {
            let sh = shared.clone();
            view.connect_load_changed(move |v, ev| {
                if ev != LoadEvent::Finished {
                    return;
                }
                let Some(appid) = sh.focus_steam_app.get() else { return };
                let uri = v.uri().map(|u| u.to_string()).unwrap_or_default();
                if !is_steam_app_page(&sh.cfg.origins.steam_store, &uri, appid) {
                    return;
                }
                sh.focus_steam_app.set(None);
                v.evaluate_javascript(STEAM_FOCUS_JS, Some("grapple-store"), None, None::<&gtk::gio::Cancellable>, |r| {
                    if let Err(e) = r {
                        tracing::debug!("store window: purchase-box script: {e}");
                    }
                });
            });
        }
    }

    fn decide(sh: &Shared, _view: &WebView, decision: &PolicyDecision, kind: PolicyDecisionType, is_popup: bool) -> bool {
        match kind {
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                let Some(nav) = decision.downcast_ref::<NavigationPolicyDecision>() else { return false };
                let Some(action) = nav.navigation_action() else { return false };
                if action.is_user_gesture() {
                    sh.last_gesture.set(Some(Instant::now()));
                }
                let uri = action.request().and_then(|r| r.uri()).map(|u| u.to_string()).unwrap_or_default();
                // Only web content. (steam://, file://, mailto: and friends never load in this window.)
                let web = Url::parse(&uri).map(|u| matches!(u.scheme(), "https" | "http" | "about" | "blob" | "data")).unwrap_or(false);
                if web {
                    // Allowed hosts load. Other hosts load too *if* they are frames: the response check below stops
                    // them in the main frame before anything is shown.
                    decision.use_();
                } else {
                    tracing::info!("store window: non-web navigation blocked");
                    decision.ignore();
                }
                true
            }
            PolicyDecisionType::Response => {
                let Some(resp) = decision.downcast_ref::<ResponsePolicyDecision>() else { return false };
                if !resp.is_mime_type_supported() {
                    tracing::info!("store window: download refused");
                    decision.ignore();
                    return true;
                }
                if !resp.is_main_frame_main_resource() {
                    decision.use_();
                    return true;
                }
                let uri = resp
                    .response()
                    .and_then(|r| r.uri())
                    .or_else(|| resp.request().and_then(|r| r.uri()))
                    .map(|u| u.to_string())
                    .unwrap_or_default();
                let recent = sh.last_gesture.get().is_some_and(|t| t.elapsed() <= GESTURE_WINDOW);
                match main_frame_decision(&sh.cfg.allow, &uri, recent, is_popup) {
                    MainFrame::Show => decision.use_(),
                    MainFrame::Browser => {
                        decision.ignore();
                        external(sh, &uri);
                    }
                    MainFrame::Block => {
                        tracing::info!("store window: {} blocked", host_of(&uri));
                        decision.ignore();
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn external(sh: &Shared, url: &str) {
        let Ok(u) = Url::parse(url) else { return };
        if !matches!(u.scheme(), "http" | "https") {
            return;
        }
        if sh.last_external.get().is_some_and(|t| t.elapsed() < EXTERNAL_GAP) {
            tracing::info!("store window: {} not opened (too many browser opens)", host_of(url));
            return;
        }
        sh.last_external.set(Some(Instant::now()));
        sh.last_gesture.set(None); // one click, one browser tab
        tracing::info!("store window: opening {} in the browser", host_of(url));
        (sh.cfg.external)(&u);
    }

    /// A `window.open` popup to an allowed page: a related view (keeps `window.opener`) with the same rules.
    fn popup(parent: &WebView, sh: &Rc<Shared>) -> WebView {
        let view = WebView::builder().related_view(parent).settings(&sh.settings).build();
        let window = toplevel(&view, 600, 760);
        wire(&view, &window, sh, true);
        let w = window.clone();
        view.connect_ready_to_show(move |_| {
            w.show_all();
            set_app_id(&w);
        });
        let w = window.clone();
        view.connect_close(move |_| w.close());
        view
    }
}

#[cfg(all(feature = "store-probe", target_os = "linux"))]
#[allow(unused_imports)] // the probe example uses it, grapple's binary does not
pub use gtk_side::probe_view;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(Checkout::parse("steam:1621690", "steam"), Ok(Checkout::Steam { appid: 1621690 }));
        assert!(Checkout::parse("steam:0", "steam").is_err());
        assert!(Checkout::parse("steam:12a", "steam").is_err());
        assert!(Checkout::parse("steam:+12", "steam").is_err());
        assert!(Checkout::parse("steam:1621690", "epic").is_err(), "store must match the key");
        assert_eq!(
            Checkout::parse("epic:d5241c76f178492ea1540fce45616757:a7ebc5e6b2de4d3a8e8ef3e3b1e4ff16", "epic"),
            Ok(Checkout::Epic {
                namespace: "d5241c76f178492ea1540fce45616757".into(),
                offer: "a7ebc5e6b2de4d3a8e8ef3e3b1e4ff16".into()
            })
        );
        assert!(Checkout::parse("epic:ns", "epic").is_err());
        assert!(Checkout::parse("epic:ns:of&x=1", "epic").is_err(), "no URL injection");
        assert!(Checkout::parse("epic:ns:of:extra", "epic").is_err());
        assert!(Checkout::parse("gog:1", "gog").is_err());
    }

    #[test]
    fn urls() {
        let o = Origins::production();
        assert_eq!(o.steam_app(1621690), "https://store.steampowered.com/app/1621690/");
        let p = o.epic_purchase("ns1", "off1");
        assert_eq!(p, "https://store.epicgames.com/purchase?offers=1-ns1-off1");
        assert_eq!(
            o.epic_exchange("abc123", &p),
            "https://www.epicgames.com/id/exchange?exchangeCode=abc123&redirectUrl=\
             https%3A%2F%2Fstore.epicgames.com%2Fpurchase%3Foffers%3D1-ns1-off1"
        );
        // the redirect survives a round trip through a URL parser
        let u = Url::parse(&o.epic_exchange("c", &p)).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().into_owned().collect();
        assert_eq!(q["redirectUrl"], p);
        assert_eq!(q["exchangeCode"], "c");
    }

    #[cfg(feature = "real")]
    #[test]
    fn secret_url_is_redacted() {
        let n = NavUrl::Secret(grapple_launch::plan::Secret::new("https://www.epicgames.com/id/exchange?exchangeCode=TOPSECRET"));
        assert!(!format!("{n:?}").contains("TOPSECRET"));
        let nav = Nav { url: n, store: "Epic Games", focus_steam_app: None };
        assert!(!format!("{nav:?}").contains("TOPSECRET"));
        assert_eq!(format!("{:?}", NavUrl::Plain("https://store.epicgames.com/purchase?offers=1-a-b".into())), "store.epicgames.com");
    }

    #[test]
    fn allow_list() {
        let a = AllowList::production();
        let ok = |s: &str| a.allows(&Url::parse(s).unwrap());
        for s in [
            "https://store.steampowered.com/app/1621690/",
            "https://store.steampowered.com/cart/",
            "https://store.steampowered.com/login/?redir=app%2F1",
            "https://checkout.steampowered.com/checkout/?accountcart=1",
            "https://login.steampowered.com/jwt/refresh",
            "https://help.steampowered.com/en/",
            "https://steamcommunity.com/login/home/",
            "https://steamcommunity.com/openid/login",
            "https://store.epicgames.com/purchase?offers=1-a-b",
            "https://www.epicgames.com/id/login",
            "https://www.paypal.com/checkoutnow?token=x",
            "about:blank",
            "blob:https://store.epicgames.com/0b6d-11",
        ] {
            assert!(ok(s), "{s} should be allowed");
        }
        for s in [
            "http://store.steampowered.com/app/1/",      // not https
            "https://store.steampowered.com:8443/",      // odd port
            "https://user@store.steampowered.com/",      // credentials in URL
            "https://steamcommunity.com/linkfilter/?u=https%3A%2F%2Fevil.example",
            "https://steamcommunity.com/id/someone",
            "https://steamcommunity.com/loginx",
            "https://store.steampowered.com.evil.example/",
            "https://evilstore.steampowered.com/",
            "https://epicgames.com/",
            "https://example.com/",
            "https://127.0.0.1/",
            "http://127.0.0.1:8080/",
            "data:text/html,hi",
            "blob:https://example.com/0b6d-11",
            "file:///etc/passwd",
            "about:config",
            "steam://run/1",
        ] {
            assert!(!ok(s), "{s} should be blocked");
        }
    }

    #[test]
    fn test_origin_override_is_exact() {
        let a = AllowList::production().with_test_origin("http://127.0.0.1:4567");
        assert!(a.allows(&Url::parse("http://127.0.0.1:4567/app/1/").unwrap()));
        assert!(!a.allows(&Url::parse("http://127.0.0.1:4568/").unwrap()));
        assert!(!a.allows(&Url::parse("https://127.0.0.1:4567/").unwrap()));
    }

    #[test]
    #[should_panic]
    fn test_origin_override_refuses_real_hosts() {
        let _ = AllowList::production().with_test_origin("http://example.com:80");
    }

    #[test]
    fn main_frame() {
        let a = AllowList::production();
        assert_eq!(main_frame_decision(&a, "https://checkout.steampowered.com/checkout/", false, false), MainFrame::Show);
        assert_eq!(main_frame_decision(&a, "https://example.com/", true, false), MainFrame::Browser);
        assert_eq!(main_frame_decision(&a, "https://example.com/", false, false), MainFrame::Block);
        assert_eq!(main_frame_decision(&a, "data:text/html,x", true, false), MainFrame::Block);
        assert_eq!(main_frame_decision(&a, "steam://run/1", true, false), MainFrame::Block);
        assert_eq!(main_frame_decision(&a, "not a url", true, false), MainFrame::Block);
        // payment popups: any https page (the card issuer's 3-D Secure host), nothing else
        assert_eq!(main_frame_decision(&a, "https://acs.somebank.co.za/3ds", false, true), MainFrame::Show);
        assert_eq!(main_frame_decision(&a, "http://acs.somebank.co.za/3ds", false, true), MainFrame::Block);
        assert_eq!(main_frame_decision(&a, "data:text/html,x", true, true), MainFrame::Block);
    }

    #[test]
    fn popups() {
        let a = AllowList::production();
        // Steam's PayPal and external-provider popups, and PayPal SDK's blank window
        assert_eq!(popup_decision(&a, "https://checkout.steampowered.com/paypal/launchauth/?x=1", "GET", false), Popup::InApp);
        assert_eq!(popup_decision(&a, "https://checkout.steampowered.com/checkout/externallink/?transid=1", "GET", true), Popup::InApp);
        assert_eq!(popup_decision(&a, "about:blank", "GET", false), Popup::InApp);
        assert_eq!(popup_decision(&a, "", "GET", false), Popup::InApp);
        // 3-D Secure: a form POST to the issuer into a named window
        assert_eq!(popup_decision(&a, "https://acs.somebank.co.za/pareq", "POST", false), Popup::InApp);
        assert_eq!(popup_decision(&a, "http://acs.somebank.co.za/pareq", "POST", true), Popup::Browser);
        // a link to another site
        assert_eq!(popup_decision(&a, "https://example.com/", "GET", true), Popup::Browser);
        assert_eq!(popup_decision(&a, "https://example.com/", "GET", false), Popup::Block);
        assert_eq!(popup_decision(&a, "steam://run/1", "GET", true), Popup::Block);
    }

    #[test]
    fn steam_app_page_match() {
        let s = "https://store.steampowered.com";
        assert!(is_steam_app_page(s, "https://store.steampowered.com/app/10/", 10));
        assert!(is_steam_app_page(s, "https://store.steampowered.com/app/10/Counter_Strike/", 10));
        assert!(is_steam_app_page(s, "https://store.steampowered.com/app/10?l=en", 10));
        assert!(!is_steam_app_page(s, "https://store.steampowered.com/app/100/", 10));
        assert!(!is_steam_app_page(s, "https://store.steampowered.com/agecheck/app/10/", 10));
    }
}
