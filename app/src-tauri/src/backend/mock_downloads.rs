//! MOCK downloads: the mockup's queue (Rust, Fall Guys, Valheim, ARK, Big Walk, Uncrashed, Brawlhalla) that moves
//! on its own. Nothing here downloads, reads a store or touches Steam. Shapes: docs/DOWNLOADS_PLAN.md (the real
//! ones live in grapple-download, which the mock build does not link).

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GB: f64 = 1_000_000_000.0;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    game: String,
    store: &'static str,
    kind: &'static str,
    state: &'static str,
    name: String,
    art: Value,
    bytes_done: u64,
    bytes_total: Option<u64>,
    bytes_per_sec: Option<u64>,
    seconds_left: Option<u64>,
    speed_history: VecDeque<u64>,
    target_dir: String,
    position: i64,
    can_pause: bool,
    can_reorder: bool,
    can_cancel: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<i64>,
    #[serde(skip)]
    base_speed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    speed_limit_bytes: u64,
    connections: u32,
    auto_update: bool,
    update_only_when_idle: bool,
    pause_while_playing: bool,
    notify_ready: bool,
    steam_default_dir: Option<String>,
    epic_default_dir: String,
    steam_bridge: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            speed_limit_bytes: 0,
            connections: 8,
            auto_update: true,
            update_only_when_idle: true,
            pause_while_playing: true,
            notify_ready: true,
            steam_default_dir: Some("/data/SteamLibrary".into()),
            epic_default_dir: "/data/Games".into(),
            steam_bridge: true,
        }
    }
}

struct Sim {
    active: Option<Item>,
    queued: Vec<Item>,
    updates: Vec<Item>,
    done: Vec<Item>,
    paused_all: bool,
    settings: Settings,
    tick: u64,
}

/// Sends an event to the UI (`downloads-changed`, `games-changed`).
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Clone)]
pub struct MockDownloads {
    sim: Arc<Mutex<Sim>>,
    emit: Emit,
}

fn now() -> i64 {
    super::now_secs()
}

impl MockDownloads {
    pub fn new(emit: Emit, names: &HashMap<String, String>, art: impl Fn(&str) -> Value) -> MockDownloads {
        let item = |game: &str, store: &'static str, kind: &'static str, state: &'static str, done: f64, total: Option<f64>, speed: f64, dir: &str, pos: i64| Item {
            game: game.into(),
            store,
            kind,
            state,
            name: names.get(game).cloned().unwrap_or_else(|| game.into()),
            art: art(game),
            bytes_done: (done * GB) as u64,
            bytes_total: total.map(|t| (t * GB) as u64),
            bytes_per_sec: None,
            seconds_left: None,
            speed_history: VecDeque::new(),
            target_dir: dir.into(),
            position: pos,
            can_pause: true,
            can_reorder: true,
            can_cancel: true,
            finished_at: None,
            base_speed: (speed * 1_000_000.0) as u64,
        };
        let mut rust = item("steam:252490", "steam", "install", "downloading", 14.2, Some(37.6), 42.8, "/data/SteamLibrary", 0);
        for v in [30, 34, 41, 38, 44, 40, 46, 42, 39, 43, 45, 41, 44, 47, 42, 40, 43, 46, 44, 42] {
            rust.speed_history.push_back(v * 1_000_000);
        }
        let mut brawl = item("steam:291550", "steam", "install", "done", 18.0, Some(18.0), 0.0, "/data/SteamLibrary", 0);
        brawl.finished_at = Some(now() - 12 * 60);
        let sim = Sim {
            active: Some(rust),
            queued: vec![
                item("epic:fallguys", "epic", "install", "queued", 0.0, Some(4.1), 31.0, "/data/Games", 1),
                item("steam:892970", "steam", "install", "queued", 0.0, Some(1.6), 38.0, "/data/SteamLibrary", 2),
                item("steam:2399830", "steam", "install", "queued", 0.0, Some(66.7), 44.0, "/data/SteamLibrary", 3),
            ],
            updates: vec![
                item("steam:1478500", "steam", "update", "queued", 0.0, Some(1.34), 30.0, "~/.local/share/Steam", 4),
                item("steam:1682970", "steam", "update", "queued", 0.0, None, 30.0, "/data/SteamLibrary", 5),
            ],
            done: vec![brawl],
            paused_all: false,
            settings: Settings::default(),
            tick: 0,
        };
        MockDownloads { sim: Arc::new(Mutex::new(sim)), emit }
    }

    /// SIMULATED progress: four steps a second while something downloads.
    pub fn start(&self) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                let moving = me.step();
                if moving {
                    (me.emit)("downloads-changed", me.snapshot());
                }
            }
        });
    }

    fn step(&self) -> bool {
        let mut s = self.sim.lock().unwrap();
        s.tick += 1;
        let tick = s.tick;
        let limit = s.settings.speed_limit_bytes;
        if s.paused_all {
            return false;
        }
        let Some(a) = s.active.as_mut() else { return false };
        if a.state != "downloading" {
            return false;
        }
        let wobble = [0, 3, -2, 5, -4, 2, -1, 4][(tick % 8) as usize];
        let mut speed = (a.base_speed as i64 + wobble * 1_000_000).max(1) as u64;
        if limit > 0 {
            speed = speed.min(limit);
        }
        a.bytes_per_sec = Some(speed);
        a.bytes_done += speed / 4;
        if tick % 4 == 0 {
            a.speed_history.push_back(speed);
            while a.speed_history.len() > 20 {
                a.speed_history.pop_front();
            }
        }
        let total = a.bytes_total.unwrap_or(u64::MAX);
        a.seconds_left = Some(total.saturating_sub(a.bytes_done) / speed.max(1));
        if a.bytes_done >= total {
            let mut d = s.active.take().unwrap();
            d.state = "done";
            d.bytes_done = total;
            d.finished_at = Some(now());
            d.bytes_per_sec = None;
            s.done.insert(0, d);
            s.done.truncate(10);
            Self::promote(&mut s);
            drop(s);
            (self.emit)("games-changed", Value::Null);
        }
        true
    }

    fn promote(s: &mut Sim) {
        if s.active.is_some() || s.paused_all {
            return;
        }
        if let Some(i) = s.queued.iter().position(|i| i.state == "queued") {
            let mut n = s.queued.remove(i);
            n.state = "downloading";
            s.active = Some(n);
        }
    }

    pub fn snapshot(&self) -> Value {
        let s = self.sim.lock().unwrap();
        json!({
            "active": s.active,
            "queued": s.queued,
            "updates": s.updates,
            "done": s.done,
            "pausedAll": s.paused_all,
            "steamBridge": if s.settings.steam_bridge { "on" } else { "off" },
        })
    }

    fn changed(&self) {
        (self.emit)("downloads-changed", self.snapshot());
    }

    pub fn install_plan(&self, id: &str, art_name: &str) -> Value {
        let epic = id.starts_with("epic:");
        let size = if id == "steam:252490" { 37.6 } else if id == "epic:fallguys" { 4.1 } else { 2.0 + (id.len() % 7) as f64 * 3.3 };
        let free_home = 2.4;
        let target = |path: &str, label: &str, free: f64, total: f64, default: bool| {
            json!({ "path": path, "label": label, "freeBytes": (free * GB) as u64, "totalBytes": (total * GB) as u64, "enough": free >= size + 1.0, "isDefault": default })
        };
        let targets = if epic {
            vec![target("~/Games/Heroic", "Home drive", free_home, 120.0, false), target("/data/Games", "Data drive", 553.0, 780.0, true)]
        } else {
            vec![target("~/.local/share/Steam", "Home drive", free_home, 120.0, false), target("/data/SteamLibrary", "Data drive", 553.0, 780.0, true)]
        };
        let mut plan = json!({ "game": id, "store": if epic { "epic" } else { "steam" }, "sizeBytes": (size * GB) as u64, "targets": targets });
        if epic && cfg!(target_os = "linux") {
            plan["protons"] = json!([{"name": "GE-Proton (latest)", "isDefault": true}, {"name": "GE-Proton10-25", "isDefault": false}, {"name": "Proton 9.0 (Beta)", "isDefault": false}]);
        }
        let _ = art_name;
        plan
    }

    pub fn install_start(&self, id: &str, target: &str, name: String, art: Value) -> Result<()> {
        {
            let mut s = self.sim.lock().unwrap();
            if s.active.as_ref().is_some_and(|a| a.game == id) || s.queued.iter().any(|i| i.game == id) {
                return Err(anyhow!("already in the queue"));
            }
            let pos = s.queued.iter().map(|i| i.position).max().unwrap_or(0) + 1;
            let epic = id.starts_with("epic:");
            s.queued.push(Item {
                game: id.into(),
                store: if epic { "epic" } else { "steam" },
                kind: "install",
                state: "queued",
                name,
                art,
                bytes_done: 0,
                bytes_total: Some((6.5 * GB) as u64),
                bytes_per_sec: None,
                seconds_left: None,
                speed_history: VecDeque::new(),
                target_dir: target.into(),
                position: pos,
                can_pause: true,
                can_reorder: true,
                can_cancel: true,
                finished_at: None,
                base_speed: 60_000_000,
            });
            Self::promote(&mut s);
        }
        self.changed();
        Ok(())
    }

    pub fn action(&self, what: &str, id: &str) -> Result<()> {
        {
            let mut s = self.sim.lock().unwrap();
            let s = &mut *s;
            match what {
                "pause" => {
                    if s.active.as_ref().is_some_and(|a| a.game == id) {
                        let mut a = s.active.take().unwrap();
                        a.state = "paused";
                        a.bytes_per_sec = None;
                        a.seconds_left = None;
                        s.queued.insert(0, a);
                        Self::promote(s);
                    } else if let Some(i) = s.queued.iter_mut().find(|i| i.game == id) {
                        i.state = "paused";
                    }
                }
                "resume" => {
                    if let Some(i) = s.queued.iter_mut().find(|i| i.game == id) {
                        i.state = "queued";
                    }
                    Self::promote(s);
                }
                "cancel" => {
                    if s.active.as_ref().is_some_and(|a| a.game == id) {
                        s.active = None;
                    }
                    s.queued.retain(|i| i.game != id);
                    s.updates.retain(|i| i.game != id);
                    Self::promote(s);
                }
                "start_now" | "update_now" => {
                    let from_q = s.queued.iter().position(|i| i.game == id).map(|i| s.queued.remove(i));
                    let from_u = s.updates.iter().position(|i| i.game == id).map(|i| s.updates.remove(i));
                    if let Some(mut n) = from_q.or(from_u) {
                        if n.bytes_total.is_none() {
                            n.bytes_total = Some((2.1 * GB) as u64);
                        }
                        if let Some(mut a) = s.active.take() {
                            a.state = "queued";
                            a.bytes_per_sec = None;
                            a.seconds_left = None;
                            s.queued.insert(0, a);
                        }
                        n.state = "downloading";
                        s.paused_all = false;
                        s.active = Some(n);
                    }
                }
                "uninstall" => {}
                _ => return Err(anyhow!("unknown action")),
            }
        }
        self.changed();
        Ok(())
    }

    pub fn reorder(&self, ids: &[String]) {
        {
            let mut s = self.sim.lock().unwrap();
            let mut q = std::mem::take(&mut s.queued);
            let mut out = Vec::new();
            for id in ids {
                if let Some(i) = q.iter().position(|x| &x.game == id) {
                    out.push(q.remove(i));
                }
            }
            out.extend(q);
            for (n, i) in out.iter_mut().enumerate() {
                i.position = n as i64 + 1;
            }
            s.queued = out;
        }
        self.changed();
    }

    pub fn pause_all(&self, paused: bool) {
        {
            let mut s = self.sim.lock().unwrap();
            s.paused_all = paused;
            if let Some(a) = s.active.as_mut() {
                a.state = if paused { "paused" } else { "downloading" };
                if paused {
                    a.bytes_per_sec = None;
                    a.seconds_left = None;
                }
            }
        }
        self.changed();
    }

    pub fn settings_get(&self) -> Value {
        let s = self.sim.lock().unwrap();
        let mut v = serde_json::to_value(&s.settings).unwrap_or_default();
        v["choices"] = json!({ "steam": ["~/.local/share/Steam", "/data/SteamLibrary"], "epic": ["~/Games/Heroic", "/data/Games"] });
        v
    }

    pub fn settings_set(&self, v: Value) -> Result<Value> {
        let mut new: Settings = serde_json::from_value(v)?;
        {
            let mut s = self.sim.lock().unwrap();
            new.steam_bridge = s.settings.steam_bridge;
            s.settings = new;
        }
        self.changed();
        Ok(self.settings_get())
    }

    pub fn bridge_set(&self, on: bool) -> Value {
        {
            let mut g = self.sim.lock().unwrap();
            let s = &mut *g;
            s.settings.steam_bridge = on;
            let steam_ctl = on;
            for i in s.queued.iter_mut().chain(s.updates.iter_mut()).chain(s.active.iter_mut()) {
                if i.store == "steam" {
                    i.can_pause = steam_ctl;
                    i.can_reorder = steam_ctl;
                    i.can_cancel = steam_ctl;
                }
            }
        }
        self.changed();
        self.settings_get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim() -> (MockDownloads, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let e = events.clone();
        let names: HashMap<String, String> = [("steam:252490", "Rust"), ("epic:fallguys", "Fall Guys")].into_iter().map(|(a, b)| (a.into(), b.into())).collect();
        let m = MockDownloads::new(Arc::new(move |ev: &str, _v| e.lock().unwrap().push(ev.to_string())), &names, |id| json!({ "hero": format!("grapple://art/{id}/hero") }));
        (m, events)
    }

    #[test]
    fn the_mockups_queue() {
        let (m, _) = sim();
        let s = m.snapshot();
        assert_eq!(s["active"]["name"], "Rust");
        assert_eq!(s["active"]["speedHistory"].as_array().unwrap().len(), 20);
        assert_eq!(s["queued"].as_array().unwrap().len(), 3);
        assert_eq!(s["queued"][0]["store"], "epic");
        assert_eq!(s["updates"][1]["bytesTotal"], Value::Null, "Uncrashed: Checking size");
        assert_eq!(s["done"][0]["state"], "done");
        assert_eq!(s["steamBridge"], "on");
    }

    #[test]
    fn commands_move_the_queue() {
        let (m, events) = sim();
        m.action("start_now", "epic:fallguys").unwrap();
        let s = m.snapshot();
        assert_eq!(s["active"]["game"], "epic:fallguys");
        assert_eq!(s["queued"][0]["game"], "steam:252490");
        m.action("pause", "epic:fallguys").unwrap();
        assert_eq!(m.snapshot()["queued"][0]["state"], "paused");
        m.reorder(&["steam:2399830".into()]);
        assert_eq!(m.snapshot()["queued"][0]["game"], "steam:2399830");
        m.pause_all(true);
        assert!(!m.step(), "nothing moves while paused");
        m.pause_all(false);
        m.action("cancel", "steam:2399830").unwrap();
        assert!(m.snapshot()["queued"].as_array().unwrap().iter().all(|i| i["game"] != "steam:2399830"));
        m.install_start("steam:730", "/data/SteamLibrary", "Counter-Strike 2".into(), json!({})).unwrap();
        assert!(m.install_start("steam:730", "/data/SteamLibrary", "x".into(), json!({})).is_err());
        let plan = m.install_plan("epic:fallguys", "");
        assert_eq!(plan["targets"][0]["enough"], false, "the full home drive");
        assert_eq!(plan["targets"][1]["isDefault"], true);
        let s = m.settings_set(json!({"speedLimitBytes": 5_000_000, "steamBridge": false})).unwrap();
        assert_eq!((s["speedLimitBytes"].as_u64(), s["steamBridge"].as_bool()), (Some(5_000_000), Some(true)), "bridge only through its own command");
        assert_eq!(m.bridge_set(false)["steamBridge"], false);
        assert!(events.lock().unwrap().iter().all(|e| e == "downloads-changed"));
    }

    #[test]
    fn a_download_finishes_and_the_next_starts() {
        let (m, events) = sim();
        m.sim.lock().unwrap().active.as_mut().unwrap().bytes_done = (37.6 * GB) as u64 - 1;
        assert!(m.step());
        let s = m.snapshot();
        assert_eq!(s["done"][0]["game"], "steam:252490");
        assert_eq!(s["active"]["game"], "epic:fallguys");
        assert!(events.lock().unwrap().contains(&"games-changed".to_string()));
    }
}
