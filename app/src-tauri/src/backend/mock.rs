//! MOCK backend. Fixture games (Josh's real 164-game list from mockups/assets/collection.json, art from the
//! files in mockups/assets) and a SIMULATED launch: nothing here starts a process, reads a store, or touches
//! a token. Switch to `real.rs` by building with `--features real` (see Cargo.toml).

use super::*;
use crate::invites::{InviteDto, InviteGameDto, InvitesSnapshot, LibGame, OpenInvite, Person, PlayAction, PlayOutcome, SendResult, decide};
use anyhow::{Result, anyhow};
use grapple_core::{Game, GameId, GameStatus, LaunchEvent};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

const COLLECTION: &str = include_str!("../../../../mockups/assets/collection.json");

#[derive(Deserialize)]
struct Row {
    id: String,
    n: String,
    i: bool,
    pt: u64,
    lp: Option<i64>,
    ic: bool,
    cv: bool,
}

/// Where the fixture art lives. Override with GRAPPLE_MOCK_ASSETS.
fn assets_dir() -> PathBuf {
    std::env::var_os("GRAPPLE_MOCK_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../mockups/assets"))
}

fn fixture_games() -> Vec<Game> {
    let dir = assets_dir();
    let mut rows: Vec<Row> = serde_json::from_str(COLLECTION).expect("collection.json");
    // Rocket League (Epic), the mockup's one non-Steam game, and Fall Guys (Epic, owned, not installed: mockup 07).
    rows.push(Row { id: "sugar".into(), n: "Rocket League".into(), i: true, pt: 2480, lp: Some(1_790_960_400), ic: true, cv: true });
    rows.push(Row { id: "fallguys".into(), n: "Fall Guys".into(), i: false, pt: 0, lp: None, ic: false, cv: true });
    let pick = |c: &[PathBuf]| c.iter().find(|p| p.exists()).cloned();
    let mut games: Vec<Game> = rows
        .into_iter()
        .map(|r| {
            let epic = r.id == "sugar" || r.id == "fallguys";
            let store = if epic { Store::Epic } else { Store::Steam };
            let sid = r.id.as_str();
            let art = Art {
                cover: r.cv.then(|| pick(&[dir.join(format!("{sid}-cover.jpg")), dir.join(format!("covers/{sid}.jpg")), dir.join(format!("dl/{sid}-tall.jpg"))])).flatten(),
                hero: pick(&[
                    dir.join(if sid == "sugar" { "sugar-hero-clean.jpg".to_string() } else { format!("{sid}-hero.jpg") }),
                    dir.join(format!("dl/{sid}-library_hero.jpg")),
                    dir.join(format!("dl/{sid}-wide.jpg")),
                ]),
                logo: pick(&[dir.join(format!("{sid}-logo.png"))]),
                icon: r.ic.then(|| pick(&[dir.join(format!("icons/{sid}.jpg"))])).flatten(),
            };
            Game {
                id: GameId::new(store, sid),
                store,
                sort_name: grapple_core::sort_name(&r.n),
                name: r.n,
                installed: r.i,
                install_dir: None,
                size_bytes: None,
                art,
                store_playtime_min: r.pt,
                last_played: r.lp,
                status: (sid == "1478500").then_some(GameStatus::UpdateQueued),
            }
        })
        .collect();
    games.sort_by(|a, b| a.sort_name.cmp(&b.sort_name));
    games
}

struct State {
    games: Vec<Game>,
    favourites: Vec<String>,
    settings: Settings,
    game_settings: HashMap<String, GameSettings>,
    art_v: HashMap<String, u64>,
    /// (game id, started_at) of the simulated session.
    running: Option<(String, i64)>,
    last_scan: Option<i64>,
}

#[derive(Clone)]
pub struct Backend {
    app: AppHandle,
    state: Arc<Mutex<State>>,
    stop: Arc<Notify>,
    pub art: Arc<ArtIndex>,
    downloads: super::mock_downloads::MockDownloads,
    invites: Arc<Mutex<MockInvites>>,
}

// ---- game invites: SIMULATED. No tether: sent invites are seen, joined or declined on a timer, and one friend
// invites Josh a little after the start. The friends are the UI's fixture friends (app/ui/src/tether/mock.ts).

const MOCK_FRIENDS: [(&str, &str, usize); 6] =
    [("rowan", "rowan", 1), ("kai", "kai", 4), ("mira", "mira", 2), ("sipho", "Sipho", 5), ("dev", "Dev", 3), ("thabo", "Thabo", 6)];

fn mock_person(id: &str) -> Person {
    let (name, slot) = MOCK_FRIENDS.iter().find(|f| f.0 == id).map_or(("Someone", 0), |f| (f.1, f.2));
    Person { id: id.into(), name: name.into(), initial: name[..1].to_uppercase(), colour: format!("var(--avatar-{slot})"), avatar: None }
}

#[derive(Default)]
struct MockInvites {
    list: Vec<InviteDto>,
    open: Option<OpenInvite>,
    from: String,
    next: u64,
}

fn ms_now() -> i64 {
    now_secs() * 1000
}

/// `grapple://art/<id>/<kind>?v=` of a fixture game (the mock serves only fixture art).
fn art_json(art: &ArtIndex, art_v: &HashMap<String, u64>, id: &str) -> serde_json::Value {
    let v = art_v.get(id).copied().unwrap_or(0);
    let enc = id.replace(':', "%3A");
    let url = |k: &str| if cfg!(windows) { format!("http://grapple.localhost/art/{enc}/{k}?v={v}") } else { format!("grapple://art/{enc}/{k}?v={v}") };
    let mut o = serde_json::Map::new();
    for (k, kind) in [("hero", ArtKind::Hero), ("cover", ArtKind::Cover)] {
        if art.path(id, kind).is_some() {
            o.insert(k.into(), url(k).into());
        }
    }
    serde_json::Value::Object(o)
}

impl Backend {
    pub fn new(app: &AppHandle) -> Result<Backend> {
        let games = fixture_games();
        let art = Arc::new(ArtIndex::default());
        let art_v = art.update(&games);
        let favourites = ["730", "1144200", "252490", "892970"].iter().map(|s| format!("steam:{s}")).collect();
        let names: HashMap<String, String> = games.iter().map(|g| (g.id.0.clone(), g.name.clone())).collect();
        let emit_app = app.clone();
        let emit: super::mock_downloads::Emit = Arc::new(move |ev: &str, v: serde_json::Value| {
            let _ = emit_app.emit(ev, v);
        });
        let downloads = super::mock_downloads::MockDownloads::new(emit, &names, |id| art_json(&art, &art_v, id));
        Ok(Backend {
            downloads,
            invites: Arc::new(Mutex::new(MockInvites { from: "friends".into(), next: 1_575_162_397_511_909_000, ..MockInvites::default() })),
            app: app.clone(),
            state: Arc::new(Mutex::new(State {
                games,
                favourites,
                settings: Settings::default(),
                game_settings: HashMap::new(),
                art_v,
                running: None,
                last_scan: Some(now_secs()),
            })),
            stop: Arc::new(Notify::new()),
            art,
        })
    }

    pub async fn list_games(&self) -> Result<Vec<GameDto>> {
        let s = self.state.lock().unwrap();
        Ok(s.games
            .iter()
            .map(|g| {
                let mut dto = GameDto::new(g, s.favourites.contains(&g.id.0), g.store_playtime_min, *s.art_v.get(&g.id.0).unwrap_or(&0));
                if s.running.as_ref().is_some_and(|(id, _)| *id == g.id.0) {
                    dto.status = Some(GameStatus::Running);
                }
                dto.hidden = s.settings.hides(&g.id.0, g.store, g.installed);
                dto
            })
            .collect())
    }

    fn stats_of(g: &Game) -> PlayStats {
        // Deterministic fake sessions, shaped like the mockup's bars.
        let ses = (g.store_playtime_min / 70).max(1);
        let mut days = [0u32; 14];
        let recent = g.last_played.is_some_and(|t| now_secs() - t < 14 * 86400);
        for (i, d) in days.iter_mut().enumerate() {
            let on = ((ses * 7 + i as u64 * 13) % 9) > 5;
            if on && recent {
                *d = (((ses + i as u64 * 5) % 7) as u32 + 2) * 12;
            }
        }
        PlayStats {
            total_min: g.store_playtime_min,
            sessions: if g.store_playtime_min == 0 { 0 } else { ses },
            longest_min: g.store_playtime_min.min(60 + (ses % 7) * 25),
            this_week_min: days[7..].iter().map(|m| *m as u64).sum(),
            last_14_days: days,
        }
    }

    pub async fn game_stats(&self, id: &str) -> Result<PlayStats> {
        let s = self.state.lock().unwrap();
        let g = s.games.iter().find(|g| g.id.0 == id).ok_or_else(|| anyhow!("unknown game"))?;
        Ok(with_week(Self::stats_of(g), &s.settings.week_start))
    }

    pub async fn library_stats(&self) -> Result<LibraryStats> {
        let s = self.state.lock().unwrap();
        let all: Vec<(String, PlayStats)> = s.games.iter().map(|g| (g.id.0.clone(), with_week(Self::stats_of(g), &s.settings.week_start))).collect();
        Ok(LibraryStats::sum(all.iter().map(|(i, st)| (i.as_str(), st))))
    }

    /// SIMULATED: emits the same events the real launcher will, with nothing behind them.
    pub async fn launch(&self, id: &str) -> Result<()> {
        let game = {
            let s = self.state.lock().unwrap();
            if s.running.is_some() {
                return Err(anyhow!("a game is already running"));
            }
            s.games.iter().find(|g| g.id.0 == id).cloned().ok_or_else(|| anyhow!("unknown game"))?
        };
        if !game.installed {
            return Err(anyhow!("{} is not installed", game.name));
        }
        let me = self.clone();
        let _ = self.app.emit("launch-started", serde_json::json!({ "game": id }));
        tauri::async_runtime::spawn(async move {
            let labels: [&str; 3] = if game.store == Store::Epic {
                ["Epic sign-in token", "Proton ready", "Starting game"]
            } else {
                ["Fetching game from Steam", "Game files ready", "Starting game"]
            };
            for (i, label) in labels.iter().enumerate() {
                tokio::time::sleep(Duration::from_millis(if i == 0 { 150 } else { 350 })).await;
                if i > 0 {
                    let _ = me.app.emit("launch-event", LaunchEvent::Step { label: labels[i - 1].into(), done: true });
                }
                let _ = me.app.emit("launch-event", LaunchEvent::Step { label: (*label).into(), done: false });
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            let _ = me.app.emit("launch-event", LaunchEvent::Step { label: labels[2].into(), done: true });
            let started = now_secs();
            me.state.lock().unwrap().running = Some((game.id.0.clone(), started));
            let _ = me.app.emit("launch-event", LaunchEvent::Running { game: game.id.clone(), started_at: started });
            me.stop.notified().await;
            let ended = now_secs();
            {
                let mut s = me.state.lock().unwrap();
                s.running = None;
                if let Some(g) = s.games.iter_mut().find(|g| g.id == game.id) {
                    g.store_playtime_min += ((ended - started) / 60) as u64;
                    g.last_played = Some(ended);
                }
            }
            let _ = me.app.emit("launch-event", LaunchEvent::Exited { game: game.id.clone(), ended_at: ended });
            let _ = me.app.emit("games-changed", ());
        });
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        if self.state.lock().unwrap().running.as_ref().is_some_and(|(r, _)| r == id) {
            self.stop.notify_one();
        }
        Ok(())
    }

    pub fn running(&self) -> Vec<(String, i64)> {
        self.state.lock().unwrap().running.iter().cloned().collect()
    }

    pub async fn set_favourite(&self, id: &str, on: bool) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.favourites.retain(|f| f != id);
        if on {
            s.favourites.push(id.to_string());
        }
        Ok(())
    }

    pub async fn rescan(&self) -> Result<()> {
        self.state.lock().unwrap().last_scan = Some(now_secs());
        let _ = self.app.emit("games-changed", ());
        Ok(())
    }

    pub fn get_settings(&self) -> Settings {
        self.state.lock().unwrap().settings.clone()
    }

    pub fn set_settings(&self, s: Settings) -> Result<Settings> {
        let before = std::mem::replace(&mut self.state.lock().unwrap().settings, s.clone());
        if s.lists_differ(&before) || s.week_start != before.week_start {
            let _ = self.app.emit("games-changed", ());
        }
        Ok(s)
    }

    /// MOCK: the fixture Epic games run through Proton.
    pub fn uses_proton(&self, id: &str) -> bool {
        id.starts_with("epic:")
    }

    pub fn get_game_settings(&self, id: &str) -> GameSettings {
        self.state.lock().unwrap().game_settings.get(id).cloned().unwrap_or_default()
    }

    pub fn set_game_settings(&self, id: &str, s: GameSettings) {
        self.state.lock().unwrap().game_settings.insert(id.to_string(), s);
    }

    /// Called once the window is up. The real backend starts the background rescan here; the mock starts its
    /// SIMULATED download queue.
    pub fn start(&self) {
        self.downloads.start();
        // A friend invites Josh a little later (the list's Play / Decline, and the toast).
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(12)).await;
            let dto = me.mock_invite("mira", false, "steam:291550", "Brawlhalla");
            me.invites.lock().unwrap().list.insert(0, dto);
            me.emit_invites();
        });
    }

    // ---- the parts the real backend implements with Steam, tether and the file picker: stand-ins

    pub async fn install(&self, id: &str) -> Result<()> {
        let plan = self.downloads.install_plan(id, "");
        let target = plan["targets"].as_array().and_then(|t| t.iter().find(|x| x["isDefault"] == true)).and_then(|t| t["path"].as_str()).unwrap_or("/data/Games").to_string();
        self.install_start(id, &target, None).await
    }

    // ---- downloads: SIMULATED (backend/mock_downloads.rs)

    fn dl_art(&self, id: &str) -> (String, serde_json::Value) {
        let s = self.state.lock().unwrap();
        let name = s.games.iter().find(|g| g.id.0 == id).map(|g| g.name.clone()).unwrap_or_else(|| id.to_string());
        (name, art_json(&self.art, &s.art_v, id))
    }

    pub async fn downloads_snapshot(&self) -> Result<serde_json::Value> {
        Ok(self.downloads.snapshot())
    }

    pub async fn install_plan(&self, id: &str) -> Result<serde_json::Value> {
        Ok(self.downloads.install_plan(id, ""))
    }

    pub async fn install_start(&self, id: &str, target_dir: &str, proton: Option<String>) -> Result<()> {
        if let Some(p) = proton.filter(|_| id.starts_with("epic:")) {
            let mut gs = self.get_game_settings(id);
            gs.proton = Some(p);
            self.set_game_settings(id, gs);
        }
        let (name, art) = self.dl_art(id);
        self.downloads.install_start(id, target_dir, name, art)
    }

    pub async fn download_action(&self, what: &str, id: &str) -> Result<()> {
        self.downloads.action(what, id)
    }

    /// MOCK: the overlay "downloads" for a second, then the switch is saved.
    pub async fn eos_overlay_set(&self, id: &str, on: bool) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let mut gs = self.get_game_settings(id);
        gs.eos_overlay = on;
        self.set_game_settings(id, gs);
        Ok(())
    }

    /// MOCK: no picked art, and picking does nothing.
    pub fn art_custom(&self, _id: &str) -> Vec<&'static str> {
        Vec::new()
    }
    pub async fn art_pick(&self, _id: &str, _kind: &str) -> Result<bool> {
        Ok(false)
    }
    pub async fn art_reset(&self, _id: &str, _kind: &str) -> Result<()> {
        Ok(())
    }

    /// MOCK: Epic games have an update, everything else is up to date.
    pub async fn check_update(&self, id: &str) -> Result<serde_json::Value> {
        Ok(serde_json::json!(if id.starts_with("epic:") { "available" } else { "upToDate" }))
    }

    pub async fn download_reorder(&self, ids: Vec<String>) -> Result<()> {
        self.downloads.reorder(&ids);
        Ok(())
    }

    pub async fn downloads_pause_all(&self, paused: bool) -> Result<()> {
        self.downloads.pause_all(paused);
        Ok(())
    }

    pub async fn download_settings_get(&self) -> Result<serde_json::Value> {
        Ok(self.downloads.settings_get())
    }

    pub async fn download_settings_set(&self, v: serde_json::Value) -> Result<serde_json::Value> {
        self.downloads.settings_set(v)
    }

    pub async fn steam_bridge_set(&self, on: bool) -> Result<serde_json::Value> {
        Ok(self.downloads.bridge_set(on))
    }

    pub async fn pick_program(&self) -> Result<Option<serde_json::Value>> {
        Ok(None)
    }

    pub fn detect_program(&self, path: &str) -> serde_json::Value {
        serde_json::json!({ "name": path.rsplit('/').next().unwrap_or(path), "path": path, "kind": "native", "label": "Native program" })
    }

    pub async fn add_manual_game(&self, _draft: ManualDraft) -> Result<String> {
        Err(anyhow!("Adding games by hand needs the real backend"))
    }

    pub async fn remove_manual_game(&self, _id: &str) -> Result<()> {
        Err(anyhow!("Adding games by hand needs the real backend"))
    }

    /// `mock: true` makes the UI show its fixture friends.
    pub fn tether_status(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({ "mock": true, "loaded": true, "signed_in": false, "connection": "off", "instance_url": "http://localhost:5173", "friends": [] }))
    }

    pub async fn tether_sign_in(&self) -> Result<serde_json::Value> {
        self.tether_status()
    }

    pub fn tether_cancel_sign_in(&self) {}

    pub async fn tether_sign_out(&self) -> Result<serde_json::Value> {
        self.tether_status()
    }

    pub async fn tether_set_instance_url(&self, _url: String) -> Result<serde_json::Value> {
        self.tether_status()
    }

    // ---- the settings page: MOCK answers (fixtures; nothing on disk is read, opened or deleted)

    pub async fn library_info(&self) -> Result<LibraryInfo> {
        let s = self.state.lock().unwrap();
        let folders = vec![
            FolderDto { path: "~/.local/share/Steam".into(), store: "steam".into(), installed: 1, free_bytes: Some(3_000_000_000) },
            FolderDto { path: "/data/SteamLibrary".into(), store: "steam".into(), installed: s.games.iter().filter(|g| g.store == Store::Steam && g.installed).count().saturating_sub(1), free_bytes: Some(552_000_000_000) },
            FolderDto { path: "~/Games/Heroic".into(), store: "epic".into(), installed: s.games.iter().filter(|g| g.store == Store::Epic && g.installed).count(), free_bytes: Some(3_000_000_000) },
        ];
        Ok(LibraryInfo { sources: SourceDto::count(&s.games), folders, last_scan: s.last_scan, hidden: hidden_named(&s.settings, &s.games) })
    }

    pub async fn protons(&self) -> Result<ProtonsDto> {
        let v = |name: &str, source: &str| ProtonDto { name: name.into(), source: source.into() };
        Ok(ProtonsDto {
            choices: vec!["GE-Proton (latest)".into(), "GE-Proton10-25".into(), "Proton-CachyOS".into(), "Proton - Experimental".into()],
            versions: vec![v("GE-Proton10-25", "heroic"), v("Proton-CachyOS", "heroic"), v("Proton - Experimental", "steam")],
            heroic_default: Some("GE-Proton10-25".into()),
        })
    }

    pub async fn accounts(&self) -> Result<AccountsDto> {
        Ok(AccountsDto { steam_found: true, steam_name: Some("player1".into()), store_data: true, epic_signed_in: true, epic_name: Some("Josh".into()) })
    }

    pub async fn store_sign_out(&self) -> Result<()> {
        Ok(())
    }

    pub async fn game_files(&self, id: &str) -> Result<GameFilesDto> {
        let s = self.state.lock().unwrap();
        let g = s.games.iter().find(|g| g.id.0 == id).ok_or_else(|| anyhow!("unknown game"))?;
        Ok(if g.installed { GameFilesDto { folder: Some(format!("~/Games/{}", g.name)), size_bytes: Some(43_700_000_000) } } else { GameFilesDto::default() })
    }

    pub async fn games_not_shared(&self) -> Result<Vec<String>> {
        Ok(self.state.lock().unwrap().game_settings.iter().filter(|(_, g)| !g.show_in_tether).map(|(id, _)| id.clone()).collect())
    }

    pub async fn storage(&self) -> Result<StorageDto> {
        Ok(StorageDto {
            cache_path: "~/.cache/grapple".into(),
            cache_bytes: 32_000_000,
            data_path: "~/.local/share/grapple".into(),
            data_bytes: 5_500_000,
            logs_path: "~/.local/state/grapple/logs".into(),
            has_log: true,
        })
    }

    pub async fn cache_clear(&self) -> Result<u64> {
        Ok(0)
    }

    pub async fn open_place(&self, _place: &str, _game: Option<&str>) -> Result<()> {
        Err(anyhow!("Opening folders needs the real backend"))
    }

    pub async fn last_launch_log(&self) -> Result<String> {
        Ok("MOCK: no game ran\n".into())
    }

    pub fn settings_reset(&self) -> Result<Settings> {
        self.set_settings(Settings::default())
    }

    /// The mock has no tether, so no pictures.
    pub fn avatar(&self, _user_id: &str) -> Option<AvatarFile> {
        None
    }

    pub fn shutdown(&self) {}

    // ---- game invites: SIMULATED

    fn library(&self) -> Vec<LibGame> {
        self.state.lock().unwrap().games.iter().map(LibGame::of).collect()
    }

    fn mock_invite(&self, friend: &str, outgoing: bool, game_id: &str, name: &str) -> InviteDto {
        let action = decide(game_id, name, &self.library());
        let mut inv = self.invites.lock().unwrap();
        inv.next += 1;
        InviteDto {
            id: inv.next.to_string(),
            outgoing,
            friend: mock_person(friend),
            game: InviteGameDto { id: game_id.into(), name: name.into() },
            state: "sent".into(),
            created_at: ms_now(),
            expires_at: ms_now() + 15 * 60_000,
            answered_at: None,
            play: (!outgoing).then(|| action.kind()),
            library_game: action.library_game().map(String::from),
        }
    }

    pub fn invites_snapshot(&self) -> InvitesSnapshot {
        let now = ms_now();
        let inv = self.invites.lock().unwrap();
        let expire = |d: &InviteDto| {
            let mut d = d.clone();
            if (d.state == "sent" || d.state == "seen") && d.expires_at <= now {
                d.state = "expired".into();
            }
            d
        };
        InvitesSnapshot { status: "ready", invites: inv.list.iter().map(expire).collect(), open: inv.open.clone() }
    }

    fn emit_invites(&self) {
        let _ = self.app.emit("invites-changed", self.invites_snapshot());
    }

    fn set_state(&self, id: &str, state: &str) {
        if let Some(d) = self.invites.lock().unwrap().list.iter_mut().find(|d| d.id == id) {
            d.state = state.into();
            if state != "seen" {
                d.answered_at = Some(ms_now());
            }
        }
        self.emit_invites();
    }

    /// SIMULATED: everyone sees it after 2 s; the first friend joins at 4 s, the second declines at 6 s, the rest
    /// never answer.
    pub async fn invite_send(&self, game: &str, friends: Vec<String>) -> Result<Vec<SendResult>> {
        let name = self.state.lock().unwrap().games.iter().find(|g| g.id.0 == game).map(|g| g.name.clone()).ok_or_else(|| anyhow!("unknown game"))?;
        let ids: Vec<String> = friends
            .iter()
            .map(|f| {
                let d = self.mock_invite(f, true, game, &name);
                let id = d.id.clone();
                self.invites.lock().unwrap().list.insert(0, d);
                id
            })
            .collect();
        self.emit_invites();
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            for id in &ids {
                if me.invites.lock().unwrap().list.iter().any(|d| d.id == *id && d.state == "sent") {
                    me.set_state(id, "seen");
                }
            }
            for (i, state) in [(0, "accepted"), (1, "declined")] {
                tokio::time::sleep(Duration::from_secs(2)).await;
                if let Some(id) = ids.get(i).filter(|id| me.invites.lock().unwrap().list.iter().any(|d| d.id == **id && d.state == "seen")) {
                    me.set_state(id, state);
                }
            }
        });
        Ok(friends.into_iter().map(|friend| SendResult { friend, ok: true, error: None }).collect())
    }

    pub async fn invite_cancel(&self, id: &str) -> Result<()> {
        self.set_state(id, "cancelled");
        Ok(())
    }

    pub async fn invite_decline(&self, id: &str) -> Result<()> {
        self.set_state(id, "declined");
        Ok(())
    }

    pub async fn invites_seen(&self, ids: Vec<String>) -> Result<()> {
        for id in ids {
            if self.invites.lock().unwrap().list.iter().any(|d| d.id == id && d.state == "sent") {
                self.set_state(&id, "seen");
            }
        }
        Ok(())
    }

    /// SIMULATED Play: decided from the fixture library like the real one; an installed game gets the simulated
    /// launch.
    pub async fn invite_accept(&self, id: &str) -> Result<PlayOutcome> {
        let (game, name) = {
            let mut inv = self.invites.lock().unwrap();
            if inv.open.as_ref().is_some_and(|o| o.id == id) {
                let open = inv.open.take().and_then(|o| o.invite).ok_or_else(|| anyhow!("This invite is no longer open"))?;
                inv.list.retain(|d| d.id != id);
                inv.list.insert(0, open);
            }
            let d = inv.list.iter().find(|d| d.id == id).ok_or_else(|| anyhow!("This invite isn't for this account"))?;
            (d.game.id.clone(), d.game.name.clone())
        };
        self.set_state(id, "accepted");
        let action = decide(&game, &name, &self.library());
        let error = match &action {
            PlayAction::Launch { game } => self.launch(game).await.err().map(|e| e.to_string()),
            _ => None,
        };
        Ok(PlayOutcome { action, name, join: None, error })
    }

    /// SIMULATED: any id opens an invite from rowan to Counter-Strike 2.
    pub fn open_invite(&self, id: String) {
        let mut dto = self.mock_invite("rowan", false, "steam:730", "Counter-Strike 2");
        dto.id = id.clone();
        self.invites.lock().unwrap().open = Some(OpenInvite { id, invite: Some(dto), error: None });
        self.emit_invites();
    }

    pub fn invite_close(&self) {
        self.invites.lock().unwrap().open = None;
        self.emit_invites();
    }

    pub async fn invite_settings_get(&self) -> Result<String> {
        Ok(self.invites.lock().unwrap().from.clone())
    }

    pub async fn invite_settings_set(&self, from: &str) -> Result<String> {
        if from != "friends" && from != "nobody" {
            return Err(anyhow!("unknown choice"));
        }
        self.invites.lock().unwrap().from = from.into();
        Ok(from.into())
    }
}
