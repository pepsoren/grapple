//! Game controllers: which pads are connected (Settings › Controllers), their live state for the Test panel, and
//! navigating grapple with a pad (`pad-nav` events; the focus logic is the UI's, app/ui/src/lib/padnav.ts).
//!
//! One thread owns the devices. It reads pads only while it has to: while the Test panel is open, or while
//! "Navigate with a controller" is on, grapple's window has the focus and no game is in front. Otherwise no
//! device is open and the thread sleeps (Linux: woken only by a pad being plugged in or out).
//!
//! Sources: Linux reads evdev directly (`evdev.rs`: no udev library, no root; a pad the user may not read is
//! left out); Windows and macOS use gilrs (`gil.rs`); the `mock` build has two sample pads (`mock.rs`).
//!
//! Commands: `controllers_list`, `controller_test_start(id)`, `controller_test_stop`, `controller_rumble(id, kind)`.
//! Events: `controllers-changed` (the list), `controller-state` (the tested pad, at most 60 per second),
//! `pad-nav` (`{ action, pad, layout }`).

pub mod nav;

#[cfg(all(feature = "real", target_os = "linux"))]
mod evdev;
#[cfg(all(feature = "real", not(target_os = "linux")))]
mod gil;
#[cfg(all(feature = "mock", not(feature = "real")))]
mod mock;

use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Listener, Manager};

/// Button bits of [`PadState::buttons`], by position (south = A on Xbox, Cross on PlayStation, B on Nintendo).
pub mod btn {
    pub const SOUTH: u32 = 1 << 0;
    pub const EAST: u32 = 1 << 1;
    pub const WEST: u32 = 1 << 2;
    pub const NORTH: u32 = 1 << 3;
    pub const LB: u32 = 1 << 4;
    pub const RB: u32 = 1 << 5;
    pub const LT: u32 = 1 << 6;
    pub const RT: u32 = 1 << 7;
    pub const SELECT: u32 = 1 << 8;
    pub const START: u32 = 1 << 9;
    pub const GUIDE: u32 = 1 << 10;
    pub const LS: u32 = 1 << 11;
    pub const RS: u32 = 1 << 12;
    pub const UP: u32 = 1 << 13;
    pub const DOWN: u32 = 1 << 14;
    pub const LEFT: u32 = 1 << 15;
    pub const RIGHT: u32 = 1 << 16;

    pub const NAMES: [(u32, &str); 17] = [
        (SOUTH, "south"),
        (EAST, "east"),
        (WEST, "west"),
        (NORTH, "north"),
        (LB, "lb"),
        (RB, "rb"),
        (LT, "lt"),
        (RT, "rt"),
        (SELECT, "select"),
        (START, "start"),
        (GUIDE, "guide"),
        (LS, "ls"),
        (RS, "rs"),
        (UP, "up"),
        (DOWN, "down"),
        (LEFT, "left"),
        (RIGHT, "right"),
    ];
}

/// A pad's inputs. Sticks -1..1 (y grows downwards), triggers 0..1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PadState {
    pub buttons: u32,
    pub lx: f32,
    pub ly: f32,
    pub rx: f32,
    pub ry: f32,
    pub lt: f32,
    pub rt: f32,
}

/// Whose buttons the pad has: picks the "Auto" button prompts, and Nintendo pads confirm with the right button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Playstation,
    Xbox,
    Nintendo,
}

impl Layout {
    /// By USB vendor id: Sony, Nintendo; everything else has Xbox-style buttons.
    pub fn of_vendor(vendor: u16) -> Layout {
        match vendor {
            0x054c => Layout::Playstation,
            0x057e => Layout::Nintendo,
            _ => Layout::Xbox,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Connection {
    /// Linux only: the bus evdev reports (gilrs on Windows only knows wired or not).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Usb,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Bluetooth,
    /// Plugged in, but the system does not say how (Windows; gilrs is part of the real backend only).
    #[cfg_attr(any(target_os = "linux", not(feature = "real")), allow(dead_code))]
    Wired,
}

/// A pad as a source sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RawPad {
    /// Stable while the pad stays connected.
    pub id: String,
    pub name: String,
    pub vendor: u16,
    pub connection: Option<Connection>,
    /// Percent, when the system reports it.
    pub battery: Option<u8>,
    pub charging: bool,
    pub rumble: bool,
}

/// A connected pad, for the UI (`controllers_list`, `controllers-changed`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PadInfo {
    pub id: String,
    pub name: String,
    pub connection: Option<Connection>,
    pub battery: Option<u8>,
    pub charging: bool,
    /// 1, 2, ...: the order the pads were connected in (a free number is reused).
    pub player: u32,
    pub layout: Layout,
    pub rumble: bool,
}

/// What a source saw while it waited.
#[derive(Debug, Default)]
pub struct Changes {
    pub list: bool,
    pub states: Vec<(String, PadState)>,
}

/// A short rumble: the Test panel's button, or the tick of "Vibrate on focus".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rumble {
    Test,
    Tick,
}

impl Rumble {
    /// (strong motor, weak motor, milliseconds)
    #[cfg_attr(not(feature = "real"), allow(dead_code))]
    pub fn params(self) -> (u16, u16, u16) {
        match self {
            Rumble::Test => (0xC000, 0xC000, 350),
            Rumble::Tick => (0, 0x5000, 35),
        }
    }
}

/// Wakes the controller thread out of [`Source::wait`].
pub type Waker = Arc<dyn Fn() + Send + Sync>;

pub trait Source: Send {
    /// The connected pads.
    fn pads(&mut self) -> Vec<RawPad>;
    /// `read`: keep the pads open and report their input. `watch`: grapple's window is on screen (sources that
    /// cannot sleep while watching for new pads stop entirely without it).
    fn set_mode(&mut self, read: bool, watch: bool);
    /// Wait up to `timeout` for input, a pad coming or going, or the waker.
    fn wait(&mut self, timeout: Duration) -> Changes;
    fn rumble(&mut self, id: &str, r: Rumble) -> bool;
    /// What every open pad shows right now (while reading).
    fn snapshot(&mut self) -> Vec<(String, PadState)>;
    fn waker(&self) -> Waker;
    /// Made-up input (the mock pads): shown in the Test panel, never used to navigate.
    fn synthetic(&self) -> bool {
        false
    }
}

fn make_source() -> Option<Box<dyn Source>> {
    #[cfg(all(feature = "real", target_os = "linux"))]
    return Some(Box::new(evdev::Evdev::new()?));
    #[cfg(all(feature = "real", not(target_os = "linux")))]
    return Some(Box::new(gil::Gil::new()));
    #[cfg(all(feature = "mock", not(feature = "real")))]
    return Some(Box::new(mock::Mock::new()));
}

/// Player numbers: a pad keeps its number while connected; a new pad gets the lowest free one.
#[derive(Debug, Default)]
pub struct Players(Vec<Option<String>>);

impl Players {
    pub fn assign(&mut self, pads: &[RawPad]) -> Vec<PadInfo> {
        for slot in self.0.iter_mut() {
            if slot.as_ref().is_some_and(|id| !pads.iter().any(|p| &p.id == id)) {
                *slot = None;
            }
        }
        pads.iter()
            .map(|p| {
                let n = match self.0.iter().position(|s| s.as_deref() == Some(p.id.as_str())) {
                    Some(i) => i,
                    None => match self.0.iter().position(Option::is_none) {
                        Some(i) => {
                            self.0[i] = Some(p.id.clone());
                            i
                        }
                        None => {
                            self.0.push(Some(p.id.clone()));
                            self.0.len() - 1
                        }
                    },
                };
                PadInfo {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    connection: p.connection,
                    battery: p.battery,
                    charging: p.charging,
                    player: n as u32 + 1,
                    layout: Layout::of_vendor(p.vendor),
                    rumble: p.rumble,
                }
            })
            .collect()
    }
}

/// Shorter names: drop the maker's name in front of the product (`Sony Interactive Entertainment DualSense
/// Wireless Controller` -> `DualSense Wireless Controller`).
#[cfg_attr(not(feature = "real"), allow(dead_code))]
pub fn clean_name(name: &str) -> String {
    let n = name.trim();
    for prefix in ["Sony Interactive Entertainment ", "Sony Computer Entertainment ", "Microsoft X-Box", "Microsoft Xbox", "Nintendo "] {
        if let Some(rest) = n.strip_prefix(prefix) {
            return if prefix.starts_with("Microsoft") { format!("Xbox{rest}") } else { rest.to_string() };
        }
    }
    if n.is_empty() { "Controller".into() } else { n.to_string() }
}

/// The tested pad's state, as `controller-state` sends it.
#[derive(Debug, Clone, Serialize)]
struct StateDto<'a> {
    id: &'a str,
    buttons: Vec<&'static str>,
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    lt: f32,
    rt: f32,
}

impl<'a> StateDto<'a> {
    fn new(id: &'a str, s: &PadState) -> Self {
        let r = |v: f32| (v * 1000.0).round() / 1000.0;
        StateDto {
            id,
            buttons: btn::NAMES.iter().filter(|(b, _)| s.buttons & b != 0).map(|(_, n)| *n).collect(),
            lx: r(s.lx),
            ly: r(s.ly),
            rx: r(s.rx),
            ry: r(s.ry),
            lt: r(s.lt),
            rt: r(s.rt),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct NavDto<'a> {
    action: nav::Action,
    pad: &'a str,
    layout: Layout,
}

/// At most this often for `controller-state` (60 per second).
const STATE_EVERY: Duration = Duration::from_millis(16);

enum Cmd {
    List(Sender<Vec<PadInfo>>),
    TestStart(String),
    TestStop,
    Rumble(String, Rumble, Sender<bool>),
    Recheck,
}

/// What decides whether pads are read, set from the window, the settings and the launch events.
#[derive(Default)]
struct Gate {
    nav_on: AtomicBool,
    focused: AtomicBool,
    visible: AtomicBool,
    /// A launch is under way (`launch-started` until running, failed or exited).
    launching: AtomicBool,
    /// Some game runs...
    playing: AtomicBool,
    /// ...but grapple's window got the focus back after it started (the person switched back to grapple).
    refocused: AtomicBool,
}

impl Gate {
    fn nav_allowed(&self) -> bool {
        let g = |a: &AtomicBool| a.load(Ordering::Relaxed);
        g(&self.nav_on) && g(&self.focused) && g(&self.visible) && !g(&self.launching) && (!g(&self.playing) || g(&self.refocused))
    }
}

pub struct Controllers {
    tx: Mutex<Sender<Cmd>>,
    wake: Waker,
    gate: Arc<Gate>,
}

impl Controllers {
    fn send(&self, c: Cmd) {
        let _ = self.tx.lock().unwrap().send(c);
        (self.wake)();
    }
}

/// Start the controller thread. Call once in `setup`, after the backend exists.
pub fn init(app: &AppHandle, nav_on: bool) {
    let Some(source) = make_source() else {
        tracing::info!("controllers: no input source on this system");
        return;
    };
    let gate = Arc::new(Gate::default());
    gate.nav_on.store(nav_on, Ordering::Relaxed);
    if let Some(w) = crate::windows::main_window(app) {
        gate.visible.store(w.is_visible().unwrap_or(false), Ordering::Relaxed);
        gate.focused.store(w.is_focused().unwrap_or(false), Ordering::Relaxed);
    }
    let (tx, rx) = channel();
    let wake = source.waker();
    app.manage(Controllers { tx: Mutex::new(tx), wake, gate: gate.clone() });
    listen_launches(app);
    let app2 = app.clone();
    let spawned = std::thread::Builder::new().name("grapple-pads".into()).spawn(move || run(app2, source, rx, gate));
    if let Err(e) = spawned {
        tracing::warn!("controllers: could not start the thread: {e}");
    }
}

fn with<R>(app: &AppHandle, f: impl FnOnce(&Controllers) -> R) -> Option<R> {
    app.try_state::<Controllers>().map(|c| f(&c))
}

/// "Navigate with a controller" changed.
pub fn set_nav(app: &AppHandle, on: bool) {
    with(app, |c| {
        c.gate.nav_on.store(on, Ordering::Relaxed);
        c.send(Cmd::Recheck);
    });
}

/// The main window's focus changed (main.rs `on_window_event`).
pub fn window_focus(window: &tauri::Window, focused: bool) {
    if window.label() != crate::windows::MAIN {
        return;
    }
    let visible = window.is_visible().unwrap_or(focused);
    with(window.app_handle(), |c| {
        c.gate.focused.store(focused, Ordering::Relaxed);
        c.gate.visible.store(visible || focused, Ordering::Relaxed);
        if focused && c.gate.playing.load(Ordering::Relaxed) && !c.gate.launching.load(Ordering::Relaxed) {
            c.gate.refocused.store(true, Ordering::Relaxed);
        }
        c.send(Cmd::Recheck);
    });
}

/// Pads never drive grapple while a game is starting, or runs in front of it.
fn listen_launches(app: &AppHandle) {
    let a = app.clone();
    app.listen_any("launch-started", move |_| {
        with(&a, |c| {
            c.gate.launching.store(true, Ordering::Relaxed);
            c.gate.refocused.store(false, Ordering::Relaxed);
            c.send(Cmd::Recheck);
        });
    });
    let a = app.clone();
    app.listen_any("launch-event", move |ev| {
        let kind = serde_json::from_str::<serde_json::Value>(ev.payload())
            .ok()
            .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(str::to_string))
            .unwrap_or_default();
        if !matches!(kind.as_str(), "running" | "exited" | "failed") {
            return;
        }
        let running = a.try_state::<Arc<crate::AppState>>().map(|s| !s.backend.running().is_empty()).unwrap_or(kind == "running");
        with(&a, |c| {
            c.gate.launching.store(false, Ordering::Relaxed);
            let was = c.gate.playing.swap(running, Ordering::Relaxed);
            if kind == "running" || !was {
                c.gate.refocused.store(false, Ordering::Relaxed);
            }
            c.send(Cmd::Recheck);
        });
    });
}

fn run(app: AppHandle, mut src: Box<dyn Source>, rx: Receiver<Cmd>, gate: Arc<Gate>) {
    let mut players = Players::default();
    let mut list: Vec<PadInfo> = players.assign(&src.pads());
    let mut states: HashMap<String, PadState> = HashMap::new();
    let mut navs: HashMap<String, nav::Nav> = HashMap::new();
    let mut test: Option<String> = None;
    // The tested pad's state waiting for its turn (the 60-per-second limit).
    let mut test_due: Option<Instant> = None;
    let mut test_sent = Instant::now() - STATE_EVERY;
    let mut reading = false;
    let nav_allowed = |src: &dyn Source| gate.nav_allowed() && !src.synthetic();
    loop {
        // Commands.
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::List(reply) => {
                    let fresh = players.assign(&src.pads());
                    if fresh != list {
                        list = fresh;
                        let _ = app.emit("controllers-changed", &list);
                    }
                    let _ = reply.send(list.clone());
                }
                Cmd::TestStart(id) => {
                    test = Some(id);
                    test_due = Some(Instant::now());
                }
                Cmd::TestStop => test = None,
                Cmd::Rumble(id, r, reply) => {
                    let _ = reply.send(src.rumble(&id, r));
                }
                Cmd::Recheck => {}
            }
        }

        let nav_on = nav_allowed(src.as_ref());
        let read = test.is_some() || nav_on;
        let watch = gate.visible.load(Ordering::Relaxed);
        src.set_mode(read, watch);
        if read != reading {
            reading = read;
            // Start every pad's navigation from what it shows now; forget old input when reading stops.
            navs.clear();
            if !read {
                states.clear();
            }
        }
        if !nav_on {
            navs.clear();
        }
        if read {
            // A pad's navigation starts from what it shows when reading starts (or it is plugged in), so a
            // button already down is not taken as a press, and the next real press is.
            for (id, s) in src.snapshot() {
                if nav_on && !navs.contains_key(&id) {
                    navs.insert(id.clone(), nav::Nav::new(&s));
                }
                if !states.contains_key(&id) {
                    if test.as_deref() == Some(id.as_str()) {
                        test_due.get_or_insert(Instant::now());
                    }
                    states.insert(id, s);
                }
            }
        }

        // How long to sleep: a held direction's repeat or the tested pad's pending state may be due.
        let now = Instant::now();
        let mut due: Vec<Instant> = navs.values().filter_map(nav::Nav::deadline).collect();
        due.extend(test_due);
        let idle = if read { Duration::from_secs(1) } else { Duration::from_secs(30) };
        let timeout = due.into_iter().min().map(|d| d.saturating_duration_since(now)).unwrap_or(idle).min(idle);

        let changes = src.wait(timeout);
        let now = Instant::now();
        if changes.list {
            let fresh = players.assign(&src.pads());
            states.retain(|id, _| fresh.iter().any(|p| &p.id == id));
            navs.retain(|id, _| fresh.iter().any(|p| &p.id == id));
            if fresh != list {
                list = fresh;
                let _ = app.emit("controllers-changed", &list);
            }
        }
        for (id, s) in changes.states {
            if nav_on {
                let layout = list.iter().find(|p| p.id == id).map(|p| p.layout).unwrap_or(Layout::Xbox);
                match navs.get_mut(&id) {
                    Some(n) => {
                        for action in n.update(&s, layout == Layout::Nintendo, now) {
                            let _ = app.emit("pad-nav", NavDto { action, pad: &id, layout });
                        }
                    }
                    None => {
                        navs.insert(id.clone(), nav::Nav::new(&s));
                    }
                }
            }
            if test.as_deref() == Some(id.as_str()) && states.get(&id) != Some(&s) {
                test_due.get_or_insert(now);
            }
            states.insert(id, s);
        }
        // Held directions repeat.
        for (id, n) in navs.iter_mut() {
            if let Some(action) = n.tick(now) {
                let layout = list.iter().find(|p| &p.id == id).map(|p| p.layout).unwrap_or(Layout::Xbox);
                let _ = app.emit("pad-nav", NavDto { action, pad: id, layout });
            }
        }
        // The tested pad, at most every STATE_EVERY.
        if let (Some(id), Some(at)) = (test.as_deref(), test_due) {
            let at = at.max(test_sent + STATE_EVERY);
            if now >= at {
                let s = states.get(id).copied().unwrap_or_default();
                let _ = app.emit("controller-state", StateDto::new(id, &s));
                test_sent = now;
                test_due = None;
            } else {
                test_due = Some(at);
            }
        } else {
            test_due = None;
        }
    }
}

// ------------------------------------------------------------------------------------------------ commands

/// The connected pads (battery read fresh).
#[tauri::command]
pub async fn controllers_list(app: AppHandle) -> Vec<PadInfo> {
    let (tx, rx) = channel();
    if with(&app, |c| c.send(Cmd::List(tx))).is_none() {
        return Vec::new();
    }
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()).await.unwrap_or_default()
}

/// Stream the pad's state as `controller-state` until `controller_test_stop`.
#[tauri::command]
pub fn controller_test_start(app: AppHandle, id: String) {
    with(&app, |c| c.send(Cmd::TestStart(id)));
}

#[tauri::command]
pub fn controller_test_stop(app: AppHandle) {
    with(&app, |c| c.send(Cmd::TestStop));
}

/// A short rumble (`kind`: `"test"`, the default, or `"tick"`). `false`: the pad cannot rumble (or is gone).
#[tauri::command]
pub async fn controller_rumble(app: AppHandle, id: String, kind: Option<String>) -> bool {
    let r = if kind.as_deref() == Some("tick") { Rumble::Tick } else { Rumble::Test };
    let (tx, rx) = channel();
    if with(&app, |c| c.send(Cmd::Rumble(id, r, tx))).is_none() {
        return false;
    }
    tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)).await.unwrap_or(false)
}

/// Does the game run through Proton (so its "Controller" choice applies)?
#[tauri::command]
pub fn game_uses_proton(state: tauri::State<'_, Arc<crate::AppState>>, id: String) -> bool {
    state.backend.uses_proton(&id)
}

/// "Open Steam's settings" (Settings › Controllers › In games): Steam's own Controller page, where Steam Input is
/// set for Steam games. The URL is the one Steam's client uses itself (steamui: `open/settings/<page>`, page
/// `controller`); it shows a Steam window.
#[tauri::command]
pub fn steam_controller_settings() -> Result<(), String> {
    #[cfg(feature = "real")]
    return grapple_launch::steam::SteamSettings::default().open_url("steam://open/settings/controller").map_err(|e| e.to_string());
    #[cfg(not(feature = "real"))]
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(id: &str, vendor: u16) -> RawPad {
        RawPad { id: id.into(), name: id.into(), vendor, connection: None, battery: None, charging: false, rumble: false }
    }

    #[test]
    fn players_keep_their_number_and_reuse_free_ones() {
        let mut p = Players::default();
        let l = p.assign(&[pad("a", 0x054c), pad("b", 0x045e)]);
        assert_eq!(l.iter().map(|x| x.player).collect::<Vec<_>>(), [1, 2]);
        assert_eq!((l[0].layout, l[1].layout), (Layout::Playstation, Layout::Xbox));
        // a leaves: b stays player 2; c gets the free 1
        let l = p.assign(&[pad("b", 0x045e), pad("c", 0x057e)]);
        assert_eq!(l.iter().map(|x| (x.id.as_str(), x.player)).collect::<Vec<_>>(), [("b", 2), ("c", 1)]);
        assert_eq!(l[1].layout, Layout::Nintendo);
        let l = p.assign(&[pad("b", 0), pad("c", 0), pad("d", 0)]);
        assert_eq!(l[2].player, 3);
    }

    #[test]
    fn names_lose_the_makers_prefix() {
        assert_eq!(clean_name("Sony Interactive Entertainment DualSense Wireless Controller"), "DualSense Wireless Controller");
        assert_eq!(clean_name("DualSense Wireless Controller"), "DualSense Wireless Controller");
        assert_eq!(clean_name("Microsoft X-Box 360 pad"), "Xbox 360 pad");
        assert_eq!(clean_name("Nintendo Switch Pro Controller"), "Switch Pro Controller");
        assert_eq!(clean_name("  "), "Controller");
    }

    #[test]
    fn state_names_the_pressed_buttons() {
        let s = PadState { buttons: btn::SOUTH | btn::LEFT, lt: 0.30049, ..PadState::default() };
        let d = StateDto::new("p", &s);
        assert_eq!(d.buttons, ["south", "left"]);
        assert_eq!(d.lt, 0.3);
    }

    #[test]
    fn settings_stored_before_controllers_still_load() {
        use crate::backend::{GameSettings, Settings};
        let s: Settings = serde_json::from_str(r#"{"autostart":true,"accent":"blue","look":"dim"}"#).unwrap();
        assert!(s.autostart);
        assert_eq!((s.pad_nav, s.pad_prompts.as_str(), s.pad_vibrate, s.pad_in_games.as_str()), (true, "auto", false, "as_is"));
        let g: GameSettings = serde_json::from_str(r#"{"gamemode":true,"show_in_tether":false}"#).unwrap();
        assert_eq!(g.pad_in_games, None);
        let g: GameSettings = serde_json::from_str(r#"{"pad_in_games":"xbox"}"#).unwrap();
        assert_eq!(g.pad_in_games.as_deref(), Some("xbox"));
    }

    #[test]
    fn the_gate_keeps_pads_out_of_games() {
        let g = Gate::default();
        for a in [&g.nav_on, &g.focused, &g.visible] {
            a.store(true, Ordering::Relaxed);
        }
        assert!(g.nav_allowed());
        g.launching.store(true, Ordering::Relaxed);
        assert!(!g.nav_allowed(), "while a launch is under way");
        g.launching.store(false, Ordering::Relaxed);
        g.playing.store(true, Ordering::Relaxed);
        assert!(!g.nav_allowed(), "while the game runs");
        g.refocused.store(true, Ordering::Relaxed);
        assert!(g.nav_allowed(), "back in grapple on purpose");
        g.focused.store(false, Ordering::Relaxed);
        assert!(!g.nav_allowed());
    }
}
