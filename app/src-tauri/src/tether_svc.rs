//! Grapple's tether service: owns the `grapple-tether` client (tokens stay in Rust, never in JS), keeps the
//! gateway connected while signed in, publishes "now playing", turns the friends' presence into the
//! snapshot the UI shows (`tether-changed`), and keeps the game invites of the last day (`invites-changed`):
//! sending, answering, the deep link's invite, and Play pressed in tether (handed to the app, which decides
//! what Play means here: `crate::invites`).
//!
//! One actor task owns all the state and handles one command at a time, so a sign-out can never race a
//! reconnect. Everything the UI can ask for goes through [`TetherService`]; Tauri is only involved through the
//! `emit` callback, which keeps this module testable without a window (see the opt-in dev:lite test below).

use crate::avatars::AvatarCache;
use crate::backend::AvatarFile;
use crate::invites::{InviteDto, InviteGameDto, InvitesSnapshot, LibGame, OpenInvite, Person, SendResult, decide};
use crate::nowplaying::{
    ArtSource, IconRecord, LibraryEntry, Playing, avatar_slot, group_id, icon_key, make_icon_png, record_matches, sha256_hex,
    steam_appid,
};
use grapple_core::Db;
use grapple_tether::{
    Browser, Error, FriendPresence, GameActivity, GameInvite, GatewayHandle, GameImage, INSTANCE_SETTING_KEY, InviteGame,
    InviteJoin, InviteState, InvitesFrom, Status, Tether, TetherConfig, TetherEvent, TetherUser,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const ME_KEY: &str = "tether.me";

// ---------------------------------------------------------------------------------------------- snapshot

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserDto {
    pub id: String,
    pub name: String,
    /// One letter for the avatar.
    pub initial: String,
    /// `var(--avatar-N)`
    pub colour: String,
    /// The profile picture (`grapple://avatar/<id>?v=<hash>`), set only once the file is cached; else the UI draws
    /// the initial.
    #[serde(default)]
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlayingDto {
    /// The library game's id when the game is in the library, else `tether:...` (groups friends in the same game).
    pub game: String,
    pub name: String,
    /// Unix seconds.
    pub since: i64,
    /// In the library?
    pub owned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FriendDto {
    pub id: String,
    pub name: String,
    pub initial: String,
    pub colour: String,
    pub avatar: Option<String>,
    pub online: bool,
    pub status: &'static str,
    pub playing: Option<PlayingDto>,
}

/// Everything the UI knows about tether. Contains no tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Snapshot {
    /// The mock backend sets this: the UI then shows its fixture friends.
    pub mock: bool,
    /// The first check (keyring, saved sign-in) has finished.
    pub loaded: bool,
    pub signed_in: bool,
    /// The browser sign-in is waiting for an answer.
    pub signing_in: bool,
    /// `off` | `connecting` | `online` | `reconnecting` | `error`
    pub connection: &'static str,
    pub error: Option<String>,
    pub instance_url: String,
    pub me: Option<UserDto>,
    pub friends: Vec<FriendDto>,
}

fn initial_of(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into())
}

pub fn user_dto(u: &TetherUser, avatars: &AvatarCache) -> UserDto {
    UserDto {
        id: u.id.clone(),
        name: u.display_name().to_string(),
        initial: initial_of(u.display_name()),
        colour: format!("var(--avatar-{})", avatar_slot(&u.id)),
        avatar: avatars.url(&u.id, u.avatar.as_deref()),
    }
}

pub fn friend_dto(f: &FriendPresence, library: &[LibraryEntry], avatars: &AvatarCache) -> FriendDto {
    let u = user_dto(&f.user, avatars);
    let playing = f.game.as_ref().map(|g| {
        let appid = match &g.image {
            Some(GameImage::Steam(id)) => steam_appid(&format!("steam:{id}")),
            _ => None,
        };
        PlayingDto {
            game: group_id(library, &g.name, appid),
            name: g.name.clone(),
            since: g.since_ms / 1000,
            owned: crate::nowplaying::match_library(library, &g.name, appid).is_some(),
        }
    });
    FriendDto {
        id: u.id,
        name: u.name,
        initial: u.initial,
        colour: u.colour,
        avatar: u.avatar,
        online: f.status != Status::Offline,
        status: match f.status {
            Status::Online => "online",
            Status::Idle => "idle",
            Status::Dnd => "dnd",
            Status::Offline => "offline",
        },
        playing,
    }
}

// ---------------------------------------------------------------------------------------------- icons

/// The PNG for a non-Steam game's tether icon, from the first usable picture. The SHA-256 of the PNG is cached
/// per game in the Db (`tether.icon.<game id>`, with the source file and its mtime) and the PNG itself in
/// `icon_dir/<sha>.png`, so a game's icon is made once and only re-made when its picture changes.
pub fn prepare_icon(db: &Db, game: &grapple_core::GameId, files: &[PathBuf], icon_dir: &Path) -> Option<Vec<u8>> {
    for src in files {
        let Ok(meta) = std::fs::metadata(src) else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        let key = icon_key(game);
        if let Ok(Some(rec)) = db.setting::<IconRecord>(&key)
            && record_matches(&rec, src, mtime)
            && let Ok(bytes) = std::fs::read(icon_dir.join(format!("{}.png", rec.sha)))
            && sha256_hex(&bytes) == rec.sha
        {
            return Some(bytes);
        }
        match make_icon_png(src) {
            Ok(png) => {
                let sha = sha256_hex(&png);
                let saved = std::fs::create_dir_all(icon_dir).is_ok() && std::fs::write(icon_dir.join(format!("{sha}.png")), &png).is_ok();
                if saved {
                    let _ = db.set_setting(&key, &IconRecord { sha, src: src.to_string_lossy().into_owned(), mtime });
                }
                return Some(png);
            }
            Err(e) => tracing::info!("tether icon: {} is not usable: {e}", src.display()),
        }
    }
    None
}

// ---------------------------------------------------------------------------------------------- service

pub struct Deps {
    pub db: Db,
    /// Builds the client for an instance (the app: the system keyring; tests: memory).
    pub make_tether: Arc<dyn Fn(TetherConfig) -> grapple_tether::Result<Tether> + Send + Sync>,
    pub browser: Arc<dyn Browser>,
    /// Called with every new snapshot (the app emits `tether-changed`).
    pub emit: Arc<dyn Fn(&Snapshot) + Send + Sync>,
    pub icon_dir: PathBuf,
    /// Where profile pictures are cached (`<user id>-<hash>.<ext>`).
    pub avatar_dir: PathBuf,
    /// The tether desktop app's `settings.json` (its chosen host is used when Grapple has no
    /// address of its own). `None` in tests.
    pub desktop_settings: Option<PathBuf>,
    /// Called with every new invites snapshot (the app emits `invites-changed`).
    pub emit_invites: Arc<dyn Fn(&InvitesSnapshot) + Send + Sync>,
    /// Josh pressed Play on an invite's card in tether (`GAME_INVITE_PLAY`): the app acts at once.
    pub play: Arc<dyn Fn(PlayRequest) + Send + Sync>,
}

/// Play pressed in tether, for the app to act on (launch, install sheet, store page).
#[derive(Debug, Clone, PartialEq)]
pub struct PlayRequest {
    pub game: grapple_tether::InviteGame,
    /// Never logged; the `Debug` of [`InviteJoin`] hides it.
    pub join: Option<InviteJoin>,
}

/// Opens the sign-in page with `xdg-open`, but with Josh's session environment rather than
/// Grapple's own (no `WEBKIT_*` / `__NV_*` / `GRAPPLE_*`), like every other program Grapple starts.
pub struct SessionBrowser;

impl Browser for SessionBrowser {
    /// Windows: the default browser through the shell (grapple sets no variables of its own there).
    #[cfg(windows)]
    fn open(&self, url: &url::Url) -> grapple_tether::Result<()> {
        crate::system::shell_open(url.as_str()).map_err(|e| Error::Browser(e.to_string()))
    }

    #[cfg(not(windows))]
    fn open(&self, url: &url::Url) -> grapple_tether::Result<()> {
        let mut child = std::process::Command::new("xdg-open")
            .arg(url.as_str())
            .env_clear()
            .envs(grapple_launch::env::for_child())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| Error::Browser(e.to_string()))?;
        // Reap it without blocking the caller.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Where the tether desktop app keeps its settings: `$XDG_CONFIG_HOME/tether/settings.json` (Electron's user data
/// folder: `%APPDATA%\tether` on Windows).
pub fn desktop_settings_path() -> Option<PathBuf> {
    Some(grapple_core::dirs::config_home().join("tether").join("settings.json"))
}

/// The host the tether desktop app is set to, if any. Only `host` is read (a string, or an
/// object with `url`); `knownHosts` is ignored on purpose (it keeps old addresses).
pub fn desktop_host(path: &std::path::Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Host {
        Url(String),
        Object { url: String },
    }
    #[derive(serde::Deserialize)]
    struct Settings {
        host: Option<Host>,
    }
    let text = std::fs::read_to_string(path).ok()?;
    let settings: Settings = serde_json::from_str(&text).ok()?;
    let url = match settings.host? {
        Host::Url(u) | Host::Object { url: u } => u,
    };
    let url = url.trim().to_string();
    (!url.is_empty()).then_some(url)
}

/// Grapple's own setting, else the desktop app's host, else the official tether.
fn resolve_instance(saved: Option<&str>, desktop: Option<&std::path::Path>) -> TetherConfig {
    if let Some(v) = saved.map(str::trim).filter(|v| !v.is_empty()) {
        if let Ok(c) = TetherConfig::new(v) {
            return c;
        }
    }
    if let Some(c) = desktop.and_then(desktop_host).and_then(|h| TetherConfig::new(&h).ok()) {
        return c;
    }
    TetherConfig::new(grapple_tether::DEFAULT_INSTANCE_URL).expect("the default instance is valid")
}

type Reply = oneshot::Sender<Result<Snapshot, String>>;

enum Cmd {
    Start,
    SignIn(Reply),
    CancelSignIn,
    SignedIn(Result<TetherUser, Error>),
    SignOut(Reply),
    SetInstance(String, Reply),
    Playing(Option<Playing>),
    Library(Vec<LibraryEntry>, Vec<LibGame>),
    Gateway(u64, TetherEvent),
    Resync(u64),
    /// A profile picture finished downloading (or failed): (user id, hash, result).
    Avatar(String, String, Result<(), String>),
    Shutdown(oneshot::Sender<()>),
    // ---- invites
    /// After a READY: the invites status and the list (or why not).
    InvitesLoaded(u64, &'static str, Vec<GameInvite>),
    /// A fresh copy from a REST answer (send, cancel, accept...).
    Upsert(GameInvite),
    /// tether answered 403 50001 to a sign-in without `invites`.
    NeedsConsent,
    /// The current client (for a REST call made outside the actor).
    Client(oneshot::Sender<Option<Arc<Tether>>>),
    /// `grapple://invite/<id>`: show that invite.
    OpenInvite(String),
    OpenFetched(String, Result<(GameInvite, Option<InviteJoin>), String>),
    CloseOpen,
    /// The `join` payload fetched for the open invite (taken once, by its Play).
    TakeJoin(String, oneshot::Sender<Option<InviteJoin>>),
    /// An open invite's time is up: publish it as expired.
    InvitesTick,
}

#[derive(Clone)]
pub struct TetherService {
    tx: mpsc::UnboundedSender<Cmd>,
    snap: Arc<RwLock<Snapshot>>,
    invites: Arc<RwLock<InvitesSnapshot>>,
    avatars: Arc<AvatarCache>,
    db: Db,
    icon_dir: PathBuf,
}

impl TetherService {
    /// Spawn the actor on the current tokio runtime. Call [`TetherService::start`] to connect.
    pub fn spawn(deps: Deps) -> TetherService {
        let (tx, rx) = mpsc::unbounded_channel();
        let snap = Arc::new(RwLock::new(Snapshot { connection: "off", ..Snapshot::default() }));
        let invites = Arc::new(RwLock::new(InvitesSnapshot::default()));
        let avatars = Arc::new(AvatarCache::new(deps.avatar_dir.clone()));
        let (db, icon_dir) = (deps.db.clone(), deps.icon_dir.clone());
        let mut actor = Actor::new(deps, tx.clone(), snap.clone(), avatars.clone());
        actor.inv_snap = invites.clone();
        // The tauri runtime in the app, the test runtime in tests: both are tokio.
        tokio::spawn(actor.run(rx));
        TetherService { tx, snap, invites, avatars, db, icon_dir }
    }

    /// The cached picture of someone in the current snapshot (for `grapple://avatar/<id>`).
    pub fn avatar(&self, user_id: &str) -> Option<AvatarFile> {
        self.avatars.file(user_id)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().unwrap().clone()
    }

    /// Connect now if a sign-in is stored.
    pub fn start(&self) {
        let _ = self.tx.send(Cmd::Start);
    }

    async fn ask(&self, make: impl FnOnce(Reply) -> Cmd) -> Result<Snapshot, String> {
        let (r, rx) = oneshot::channel();
        self.tx.send(make(r)).map_err(|_| "tether is not running".to_string())?;
        rx.await.map_err(|_| "tether is not running".to_string())?
    }

    /// Browser sign-in. Resolves when the person answered (or it timed out / was cancelled).
    pub async fn sign_in(&self) -> Result<Snapshot, String> {
        self.ask(Cmd::SignIn).await
    }

    pub fn cancel_sign_in(&self) {
        let _ = self.tx.send(Cmd::CancelSignIn);
    }

    pub async fn sign_out(&self) -> Result<Snapshot, String> {
        self.ask(Cmd::SignOut).await
    }

    pub async fn set_instance_url(&self, url: String) -> Result<Snapshot, String> {
        self.ask(|r| Cmd::SetInstance(url, r)).await
    }

    /// What Josh is playing now (`None` clears it). Remembered, so it is sent again after a reconnect or sign-in.
    pub fn set_playing(&self, playing: Option<Playing>) {
        let _ = self.tx.send(Cmd::Playing(playing));
    }

    /// The library, for mapping friends' games to games you have (and what Play on an invite means here).
    pub fn set_library(&self, games: &[grapple_core::Game]) {
        let _ = self.tx.send(Cmd::Library(games.iter().map(LibraryEntry::of).collect(), games.iter().map(LibGame::of).collect()));
    }

    // ---- invites (the REST calls run here, outside the actor; their answers go back to it)

    pub fn invites_snapshot(&self) -> InvitesSnapshot {
        self.invites.read().unwrap().clone()
    }

    async fn client(&self) -> Result<Arc<Tether>, String> {
        let (r, rx) = oneshot::channel();
        self.tx.send(Cmd::Client(r)).map_err(|_| "tether is not running".to_string())?;
        rx.await.ok().flatten().ok_or_else(|| "Sign in with tether first".to_string())
    }

    /// The error as the UI shows it; a missing `invites` scope also turns on "Allow invites".
    fn invite_error(&self, e: Error) -> String {
        if matches!(e, Error::NeedsConsent) {
            let _ = self.tx.send(Cmd::NeedsConsent);
        }
        e.to_string()
    }

    /// Invite friends to a game: one invite per friend, open for 15 minutes. A non-Steam game's picture is
    /// uploaded first (`icon:<sha256>`, as for "now playing"); without one the card draws a plain tile.
    pub async fn send_invites(&self, game_id: &str, name: &str, art: ArtSource, friends: &[String]) -> Result<Vec<SendResult>, String> {
        let t = self.client().await?;
        let image = match art {
            ArtSource::Steam(app) => Some(format!("steam:{app}")),
            ArtSource::Files(files) => {
                let (db, dir, id) = (self.db.clone(), self.icon_dir.clone(), grapple_core::GameId(game_id.to_string()));
                let png = tokio::task::spawn_blocking(move || prepare_icon(&db, &id, &files, &dir)).await.ok().flatten();
                match png {
                    Some(png) => t.api().ensure_icon(&png).await.ok().map(|sha| format!("icon:{sha}")),
                    None => None,
                }
            }
        };
        let game = InviteGame::new(game_id, name, image.as_deref()).map_err(|e| e.to_string())?;
        let names: HashMap<String, String> = self.snapshot().friends.into_iter().map(|f| (f.id, f.name)).collect();
        let mut out = Vec::with_capacity(friends.len());
        let mut stop: Option<String> = None;
        for friend in friends {
            if let Some(why) = &stop {
                out.push(SendResult { friend: friend.clone(), ok: false, error: Some(why.clone()) });
                continue;
            }
            match t.api().send_invite(friend, &game, Some(grapple_tether::INVITE_EXPIRES_DEFAULT_SECS)).await {
                Ok(invite) => {
                    let _ = self.tx.send(Cmd::Upsert(invite));
                    out.push(SendResult { friend: friend.clone(), ok: true, error: None });
                }
                Err(e) => {
                    let who = names.get(friend).cloned().unwrap_or_else(|| "them".into());
                    let why = match e {
                        Error::CannotInvite | Error::UnknownUser => format!("Can't invite {who}"),
                        // The same for everyone after this one.
                        e @ (Error::NeedsConsent | Error::RateLimited { .. } | Error::SignedOut | Error::NotSignedIn) => {
                            let why = self.invite_error(e);
                            stop = Some(why.clone());
                            why
                        }
                        e => self.invite_error(e),
                    };
                    out.push(SendResult { friend: friend.clone(), ok: false, error: Some(why) });
                }
            }
        }
        Ok(out)
    }

    pub async fn cancel_invite(&self, id: &str) -> Result<(), String> {
        let t = self.client().await?;
        let invite = t.api().cancel_invite(id).await.map_err(|e| self.invite_error(e))?;
        let _ = self.tx.send(Cmd::Upsert(invite));
        Ok(())
    }

    pub async fn decline_invite(&self, id: &str) -> Result<(), String> {
        let t = self.client().await?;
        let invite = t.api().decline_invite(id).await.map_err(|e| self.invite_error(e))?;
        let _ = self.tx.send(Cmd::Upsert(invite));
        Ok(())
    }

    /// Invites to Josh that grapple just showed (its Invites list): `sent` → `seen`, for the sender.
    pub async fn invites_seen(&self, ids: &[String]) -> Result<(), String> {
        let t = self.client().await?;
        for id in ids {
            match t.api().invite_seen(id).await {
                Ok(invite) => {
                    let _ = self.tx.send(Cmd::Upsert(invite));
                }
                Err(e) => tracing::info!("tether: could not mark invite {id} seen: {e}"),
            }
        }
        Ok(())
    }

    /// Play on an invite in grapple (its list, or the deep link's invite): accept it with the app token (tether
    /// tells nobody else; grapple acts itself). Returns the game and the `join` payload, if one came with it.
    pub async fn accept_invite(&self, id: &str) -> Result<(InviteGame, Option<InviteJoin>), String> {
        let t = self.client().await?;
        let (r, rx) = oneshot::channel();
        let _ = self.tx.send(Cmd::TakeJoin(id.to_string(), r));
        let mut join = rx.await.ok().flatten();
        let accepted = t.api().accept_invite(id).await.map_err(|e| self.invite_error(e))?;
        if join.is_none() && accepted.invite.joinable {
            join = t.api().invite(id).await.ok().and_then(|(_, j)| j);
        }
        let game = accepted.invite.game.clone();
        let _ = self.tx.send(Cmd::Upsert(accepted.invite));
        let _ = self.tx.send(Cmd::CloseOpen);
        Ok((game, join))
    }

    /// `grapple://invite/<id>` (already checked): show that invite (after the sign-in, if there is none yet).
    pub fn open_invite(&self, id: String) {
        let _ = self.tx.send(Cmd::OpenInvite(id));
    }

    pub fn close_invite(&self) {
        let _ = self.tx.send(Cmd::CloseOpen);
    }

    /// "Who can invite me": `friends` or `nobody`.
    pub async fn invite_settings(&self) -> Result<InvitesFrom, String> {
        let t = self.client().await?;
        t.api().invite_settings().await.map_err(|e| self.invite_error(e))
    }

    pub async fn set_invite_settings(&self, from: InvitesFrom) -> Result<InvitesFrom, String> {
        let t = self.client().await?;
        t.api().set_invite_settings(from).await.map_err(|e| self.invite_error(e))
    }

    /// Close the gateway cleanly (tether forgets the session and the game). Waits at most `wait`.
    pub async fn shutdown(&self, wait: Duration) {
        let (r, rx) = oneshot::channel();
        if self.tx.send(Cmd::Shutdown(r)).is_ok() {
            let _ = tokio::time::timeout(wait, rx).await;
        }
    }
}

// ---------------------------------------------------------------------------------------------- actor

struct Actor {
    deps: Deps,
    tx: mpsc::UnboundedSender<Cmd>,
    snap: Arc<RwLock<Snapshot>>,
    config: TetherConfig,
    tether: Option<Arc<Tether>>,
    gateway: Option<GatewayHandle>,
    pump: Option<JoinHandle<()>>,
    generation: u64,
    signin_task: Option<JoinHandle<()>>,
    signin_waiters: Vec<Reply>,
    playing: Option<Playing>,
    library: Vec<LibraryEntry>,
    me: Option<UserDto>,
    /// Who is in the snapshot, for their pictures.
    me_user: Option<TetherUser>,
    avatars: Arc<AvatarCache>,
    /// user id -> picture hash, for everyone now in the snapshot who has one.
    wanted_avatars: HashMap<String, String>,
    avatars_inflight: HashSet<(String, String)>,
    avatars_failed: HashMap<(String, String), Instant>,
    /// At most a few downloads at once.
    avatar_slots: Arc<tokio::sync::Semaphore>,
    // ---- invites
    inv_snap: Arc<RwLock<InvitesSnapshot>>,
    /// `off | unsupported | needs_consent | ready` (see [`InvitesSnapshot::status`]).
    invites_status: &'static str,
    book: InviteBook,
    lib_games: Vec<LibGame>,
    /// The invite a deep link opened.
    open: Option<OpenState>,
    /// Wakes the actor when the next open invite runs out.
    invites_tick: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct OpenState {
    id: String,
    invite: Option<GameInvite>,
    /// Only for the recipient, only until Play; never published.
    join: Option<InviteJoin>,
    error: Option<String>,
    fetching: bool,
}

/// A failed picture is not asked for again for this long (presence changes would otherwise retry every time).
const AVATAR_RETRY: Duration = Duration::from_secs(300);
const AVATAR_PARALLEL: usize = 3;

impl Actor {
    fn new(deps: Deps, tx: mpsc::UnboundedSender<Cmd>, snap: Arc<RwLock<Snapshot>>, avatars: Arc<AvatarCache>) -> Actor {
        let saved: Option<String> = deps.db.setting(INSTANCE_SETTING_KEY).ok().flatten();
        let config = resolve_instance(saved.as_deref(), deps.desktop_settings.as_deref());
        let me = deps.db.setting::<UserDto>(ME_KEY).ok().flatten();
        let a = Actor {
            deps,
            tx,
            snap,
            config,
            tether: None,
            gateway: None,
            pump: None,
            generation: 0,
            signin_task: None,
            signin_waiters: Vec::new(),
            playing: None,
            library: Vec::new(),
            me,
            me_user: None,
            avatars,
            wanted_avatars: HashMap::new(),
            avatars_inflight: HashSet::new(),
            avatars_failed: HashMap::new(),
            avatar_slots: Arc::new(tokio::sync::Semaphore::new(AVATAR_PARALLEL)),
            inv_snap: Arc::default(),
            invites_status: "off",
            book: InviteBook::default(),
            lib_games: Vec::new(),
            open: None,
            invites_tick: None,
        };
        a.publish(|s| s.instance_url = a_origin(&a.config));
        a
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Start => self.start().await,
                Cmd::SignIn(reply) => self.begin_sign_in(reply),
                Cmd::CancelSignIn => self.cancel_sign_in(),
                Cmd::SignedIn(r) => self.signed_in(r).await,
                Cmd::SignOut(reply) => {
                    let r = self.sign_out().await;
                    let _ = reply.send(r);
                }
                Cmd::SetInstance(url, reply) => {
                    let r = self.set_instance(&url).await;
                    let _ = reply.send(r);
                }
                Cmd::Playing(p) => {
                    self.playing = p;
                    self.publish_game().await;
                }
                Cmd::Library(l, games) => {
                    self.library = l;
                    self.lib_games = games;
                    self.refresh_friends();
                    self.publish_invites();
                }
                Cmd::Gateway(gen_, ev) if gen_ == self.generation => self.on_event(ev).await,
                Cmd::Resync(gen_) if gen_ == self.generation => self.refresh_friends(),
                Cmd::Avatar(id, hash, result) => self.avatar_done(id, hash, result),
                Cmd::Gateway(..) | Cmd::Resync(_) => {}
                Cmd::Shutdown(done) => {
                    self.disconnect().await;
                    let _ = done.send(());
                    return;
                }
                Cmd::InvitesLoaded(gen_, status, list) if gen_ == self.generation => {
                    self.invites_status = status;
                    self.book.replace(list);
                    self.publish_invites();
                    self.fetch_open();
                }
                Cmd::InvitesLoaded(..) => {}
                Cmd::Upsert(invite) => self.invite_changed(invite),
                Cmd::NeedsConsent => {
                    if self.invites_status != "off" {
                        self.invites_status = "needs_consent";
                        self.publish_invites();
                    }
                }
                Cmd::Client(reply) => {
                    let _ = reply.send(if self.invites_status == "off" { None } else { self.tether.clone() });
                }
                Cmd::OpenInvite(id) => {
                    self.open = Some(OpenState { id, ..OpenState::default() });
                    self.publish_invites();
                    self.fetch_open();
                }
                Cmd::OpenFetched(id, r) => self.open_fetched(id, r),
                Cmd::CloseOpen => {
                    self.open = None;
                    self.publish_invites();
                }
                Cmd::TakeJoin(id, reply) => {
                    let join = self.open.as_mut().filter(|o| o.id == id).and_then(|o| o.join.take());
                    let _ = reply.send(join);
                }
                Cmd::InvitesTick => self.publish_invites(),
            }
        }
    }

    // ---- snapshot

    /// Change the snapshot and tell the UI.
    fn publish(&self, f: impl FnOnce(&mut Snapshot)) {
        let snap = {
            let mut s = self.snap.write().unwrap();
            f(&mut s);
            s.clone()
        };
        (self.deps.emit)(&snap);
    }

    fn refresh_friends(&mut self) {
        let presences = self.gateway.as_ref().map(|g| g.friends()).unwrap_or_default();
        self.sync_avatars(&presences);
        let friends: Vec<FriendDto> = presences.iter().map(|f| friend_dto(f, &self.library, &self.avatars)).collect();
        self.publish(|s| s.friends = friends);
    }

    fn remember_me(&mut self, me: Option<TetherUser>) {
        self.me_user = me.clone();
        let presences = self.gateway.as_ref().map(|g| g.friends()).unwrap_or_default();
        self.sync_avatars(&presences);
        let dto = me.as_ref().map(|u| user_dto(u, &self.avatars));
        if let Some(m) = &dto {
            // The picture URL is only good while the file is cached for this run: never keep it.
            let _ = self.deps.db.set_setting(ME_KEY, &UserDto { avatar: None, ..m.clone() });
        }
        self.me = dto.clone();
        self.publish(|s| s.me = dto);
    }

    // ---- profile pictures

    /// Serve only the people in the snapshot, and start downloading the pictures that are not cached yet. Never
    /// waits: each download is its own task, and [`Actor::avatar_done`] publishes again when one lands.
    fn sync_avatars(&mut self, friends: &[FriendPresence]) {
        let wanted: HashMap<String, String> = self
            .me_user
            .iter()
            .chain(friends.iter().map(|f| &f.user))
            .filter_map(|u| Some((u.id.clone(), u.avatar.clone()?)))
            .filter(|(id, hash)| grapple_tether::valid_user_id(id) && grapple_tether::valid_avatar_hash(hash))
            .collect();
        self.avatars.retain(&wanted);
        let Some(tether) = self.tether.clone() else {
            self.wanted_avatars = wanted;
            return;
        };
        for user in self.me_user.iter().chain(friends.iter().map(|f| &f.user)) {
            let (Some(hash), true) = (user.avatar.clone(), wanted.contains_key(&user.id)) else { continue };
            let key = (user.id.clone(), hash.clone());
            if self.avatars.is_ready(&user.id, &hash) || self.avatars_inflight.contains(&key) {
                continue;
            }
            if self.avatars_failed.get(&key).is_some_and(|at| at.elapsed() < AVATAR_RETRY) {
                continue;
            }
            self.avatars_inflight.insert(key);
            let (cache, tx, slots, tether, user) = (self.avatars.clone(), self.tx.clone(), self.avatar_slots.clone(), tether.clone(), user.clone());
            tokio::spawn(async move {
                let _slot = slots.acquire().await;
                let hash = user.avatar.clone().unwrap_or_default();
                let r = cache.ensure(&user.id, &hash, || async { tether.api().avatar(&user).await.map_err(|e| e.to_string()) }).await;
                let _ = tx.send(Cmd::Avatar(user.id.clone(), hash, r));
            });
        }
        self.wanted_avatars = wanted;
    }

    fn avatar_done(&mut self, id: String, hash: String, result: Result<(), String>) {
        let key = (id.clone(), hash.clone());
        self.avatars_inflight.remove(&key);
        match result {
            Ok(()) => {
                self.avatars_failed.remove(&key);
                // Only if this person and picture are still in the snapshot.
                if self.wanted_avatars.get(&id) == Some(&hash) && self.avatars.mark_ready(&id, &hash) {
                    self.republish_people();
                }
            }
            Err(why) => {
                tracing::info!("tether: no picture for user {id}: {why}");
                self.avatars_failed.insert(key, Instant::now());
            }
        }
    }

    /// Publish me and the friends again (a picture arrived).
    fn republish_people(&mut self) {
        let me = self.me_user.as_ref().map(|u| user_dto(u, &self.avatars));
        self.me = me.clone();
        let presences = self.gateway.as_ref().map(|g| g.friends()).unwrap_or_default();
        let friends: Vec<FriendDto> = presences.iter().map(|f| friend_dto(f, &self.library, &self.avatars)).collect();
        self.publish(|s| {
            if s.me.is_some() {
                s.me = me;
            }
            s.friends = friends;
        });
    }

    /// Nobody is in the snapshot any more: stop serving pictures.
    fn forget_people(&mut self) {
        self.me_user = None;
        self.wanted_avatars.clear();
        self.avatars.retain(&HashMap::new());
    }

    // ---- connecting

    async fn start(&mut self) {
        self.rebuild_tether().await;
        self.publish(|s| s.loaded = true);
    }

    /// (Re)create the client for `self.config` and connect if it holds a sign-in.
    async fn rebuild_tether(&mut self) {
        self.disconnect().await;
        let origin = a_origin(&self.config);
        match (self.deps.make_tether)(self.config.clone()) {
            Err(e) => {
                self.tether = None;
                let msg = e.to_string();
                self.publish(|s| {
                    s.instance_url = origin;
                    s.loaded = true;
                    s.signed_in = false;
                    s.connection = "error";
                    s.error = Some(msg);
                });
            }
            Ok(t) => {
                let t = Arc::new(t);
                self.tether = Some(t.clone());
                match t.is_signed_in().await {
                    Ok(true) => {
                        self.publish(|s| {
                            s.instance_url = origin;
                            s.loaded = true;
                            s.signed_in = true;
                            s.error = None;
                        });
                        self.connect().await;
                    }
                    Ok(false) => self.publish(|s| {
                        s.instance_url = origin;
                        s.loaded = true;
                        s.signed_in = false;
                        s.connection = "off";
                        s.error = None;
                    }),
                    Err(e) => {
                        let msg = e.to_string();
                        self.publish(|s| {
                            s.instance_url = origin;
                            s.loaded = true;
                            s.signed_in = false;
                            s.connection = "error";
                            s.error = Some(msg);
                        });
                    }
                }
            }
        }
    }

    async fn connect(&mut self) {
        let Some(t) = self.tether.clone() else { return };
        self.stop_gateway().await;
        let gw = t.connect();
        self.generation += 1;
        let gen_ = self.generation;
        let mut rx = gw.subscribe();
        let tx = self.tx.clone();
        self.pump = Some(tokio::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if tx.send(Cmd::Gateway(gen_, ev)).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        if tx.send(Cmd::Resync(gen_)).is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        }));
        self.gateway = Some(gw);
        self.publish(|s| {
            s.signed_in = true;
            s.connection = "connecting";
        });
        self.publish_game().await;
    }

    async fn stop_gateway(&mut self) {
        if let Some(p) = self.pump.take() {
            p.abort();
        }
        self.generation += 1; // events of the old connection are ignored from here on
        if let Some(g) = self.gateway.take() {
            g.shutdown().await;
        }
    }

    /// Close the connection and forget the friends (the sign-in itself stays in the store).
    async fn disconnect(&mut self) {
        self.stop_gateway().await;
        self.publish(|s| {
            s.connection = "off";
            s.friends.clear();
        });
        self.sync_avatars(&[]); // only Josh's own picture stays served
        self.forget_invites();
    }

    async fn on_event(&mut self, ev: TetherEvent) {
        match ev {
            TetherEvent::Ready { me, .. } => {
                self.remember_me(Some(me));
                self.publish(|s| {
                    s.connection = "online";
                    s.error = None;
                });
                self.load_invites();
            }
            TetherEvent::InviteCreated(invite) | TetherEvent::InviteUpdated(invite) => {
                self.invite_changed(invite);
                return;
            }
            TetherEvent::InvitePlay { invite, join } => {
                let game = invite.game.clone();
                self.invite_changed(invite);
                (self.deps.play)(PlayRequest { game, join });
                return;
            }
            TetherEvent::Resumed => self.publish(|s| s.connection = "online"),
            TetherEvent::Friend(_) | TetherEvent::FriendRemoved { .. } => {}
            TetherEvent::Disconnected => self.publish(|s| s.connection = "reconnecting"),
            TetherEvent::SignedOut => {
                self.stop_gateway().await;
                self.me = None;
                self.forget_people();
                self.forget_invites();
                self.publish(|s| {
                    s.signed_in = false;
                    s.connection = "off";
                    s.me = None;
                    s.friends.clear();
                    s.error = Some("The tether sign-in ended. Sign in again.".into());
                });
                return;
            }
            TetherEvent::Fatal(why) => {
                self.publish(|s| {
                    s.connection = "error";
                    s.error = Some(why);
                });
                return;
            }
        }
        self.refresh_friends();
    }

    // ---- invites

    /// After a READY (a new session: events may have been missed): does this sign-in have `invites`, and the
    /// list of the last day. A sign-in without the scope asks whether this tether has invites at all ("Allow
    /// invites", or nothing to show).
    fn load_invites(&mut self) {
        let (Some(t), Some(gw)) = (self.tether.clone(), self.gateway.as_ref()) else { return };
        let ready_scopes = gw.scopes();
        let (tx, gen_) = (self.tx.clone(), self.generation);
        tokio::spawn(async move {
            let scopes = match ready_scopes {
                Some(s) => s,
                None => t.scopes().await.unwrap_or_default(),
            };
            let (status, list) = if scopes.iter().any(|s| s == "invites") {
                match t.api().invites(100).await {
                    Ok(list) => ("ready", list),
                    Err(Error::NeedsConsent) => ("needs_consent", Vec::new()),
                    Err(e) => {
                        tracing::warn!("tether: could not read the invites: {e}");
                        ("ready", Vec::new())
                    }
                }
            } else if t.api().invites_supported().await {
                ("needs_consent", Vec::new())
            } else {
                ("unsupported", Vec::new())
            };
            let _ = tx.send(Cmd::InvitesLoaded(gen_, status, list));
        });
    }

    fn invite_changed(&mut self, invite: GameInvite) {
        if let Some(o) = self.open.as_mut().filter(|o| o.id == invite.id) {
            o.invite = Some(invite.clone());
        }
        self.book.upsert(invite);
        self.publish_invites();
    }

    fn forget_invites(&mut self) {
        self.invites_status = "off";
        self.book = InviteBook::default();
        // A deep link's invite stays asked for (it loads after the sign-in); what was fetched goes.
        if let Some(o) = self.open.as_mut() {
            *o = OpenState { id: std::mem::take(&mut o.id), ..OpenState::default() };
        }
        self.publish_invites();
    }

    /// Fetch the deep link's invite (with its `join` payload) once invites are ready.
    fn fetch_open(&mut self) {
        let Some(t) = self.tether.clone().filter(|_| self.invites_status == "ready") else { return };
        let Some(o) = self.open.as_mut().filter(|o| o.invite.is_none() && !o.fetching) else { return };
        o.fetching = true;
        o.error = None;
        let (tx, id) = (self.tx.clone(), o.id.clone());
        tokio::spawn(async move {
            let r = t.api().invite(&id).await;
            if let Err(Error::NeedsConsent) = &r {
                let _ = tx.send(Cmd::NeedsConsent);
            }
            let _ = tx.send(Cmd::OpenFetched(id, r.map_err(|e| e.to_string())));
        });
    }

    fn open_fetched(&mut self, id: String, r: Result<(GameInvite, Option<InviteJoin>), String>) {
        let Some(o) = self.open.as_mut().filter(|o| o.id == id) else { return };
        o.fetching = false;
        match r {
            Ok((invite, join)) => {
                // Shown now: tell tether (and the sender) it was seen.
                let me = self.me.as_ref().map(|m| m.id.clone()).unwrap_or_default();
                if invite.recipient_id == me && invite.state == InviteState::Sent && invite.is_open(now_ms())
                    && let Some(t) = self.tether.clone()
                {
                    let (tx, id) = (self.tx.clone(), invite.id.clone());
                    tokio::spawn(async move {
                        if let Ok(seen) = t.api().invite_seen(&id).await {
                            let _ = tx.send(Cmd::Upsert(seen));
                        }
                    });
                }
                o.join = join;
                o.invite = Some(invite.clone());
                self.book.upsert(invite);
            }
            Err(why) => o.error = Some(why),
        }
        self.publish_invites();
    }

    fn person(&self, user_id: &str) -> Person {
        let friends = self.gateway.as_ref().map(|g| g.friends()).unwrap_or_default();
        match friends.iter().find(|f| f.user.id == user_id) {
            Some(f) => {
                let u = user_dto(&f.user, &self.avatars);
                Person { id: u.id, name: u.name, initial: u.initial, colour: u.colour, avatar: u.avatar }
            }
            None => Person { id: user_id.to_string(), name: "Someone".into(), initial: "?".into(), colour: "var(--avatar-offline)".into(), avatar: None },
        }
    }

    fn publish_invites(&mut self) {
        let now = now_ms();
        let me = self.me.as_ref().map(|m| m.id.clone()).unwrap_or_default();
        let person = |id: &str| self.person(id);
        let invites = if self.invites_status == "off" { Vec::new() } else { self.book.dtos(&me, now, &person, &self.lib_games) };
        let open = self.open.as_ref().map(|o| OpenInvite {
            id: o.id.clone(),
            invite: o.invite.as_ref().map(|i| invite_dto(i, &me, now, &person, &self.lib_games)),
            error: o.error.clone(),
        });
        let snap = InvitesSnapshot { status: self.invites_status, invites, open };
        *self.inv_snap.write().unwrap() = snap.clone();
        (self.deps.emit_invites)(&snap);
        // Wake up when the next open invite runs out, so it shows as expired without waiting for tether.
        if let Some(t) = self.invites_tick.take() {
            t.abort();
        }
        if let Some(at) = self.book.next_expiry(now) {
            let tx = self.tx.clone();
            let wait = Duration::from_millis((at - now).clamp(0, 3_600_000) as u64 + 50);
            self.invites_tick = Some(tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                let _ = tx.send(Cmd::InvitesTick);
            }));
        }
    }

    // ---- now playing

    async fn publish_game(&mut self) {
        let Some(gw) = &self.gateway else { return };
        let activity = match self.playing.clone() {
            None => None,
            Some(p) => Some(match &p.art {
                ArtSource::Steam(app) => GameActivity::steam(p.name.clone(), *app, p.started_ms()),
                ArtSource::Files(files) => {
                    let (db, dir, files, id) = (self.deps.db.clone(), self.deps.icon_dir.clone(), files.clone(), p.game.clone());
                    let png = tokio::task::spawn_blocking(move || prepare_icon(&db, &id, &files, &dir)).await.ok().flatten();
                    match png {
                        Some(png) => GameActivity::with_icon(p.name.clone(), png, p.started_ms()),
                        None => GameActivity::plain(p.name.clone(), p.started_ms()),
                    }
                }
            }),
        };
        if let Err(e) = gw.set_game(activity).await {
            tracing::warn!("tether: could not publish the game: {e}");
            if matches!(e, Error::SignedOut | Error::NotSignedIn) {
                let _ = self.tx.send(Cmd::Gateway(self.generation, TetherEvent::SignedOut));
            }
        }
    }

    // ---- sign in / out

    fn begin_sign_in(&mut self, reply: Reply) {
        if self.signin_task.is_some() {
            self.signin_waiters.push(reply);
            return;
        }
        let Some(t) = self.tether.clone() else {
            let why = self.snap.read().unwrap().error.clone().unwrap_or_else(|| "tether is not set up".into());
            let _ = reply.send(Err(why));
            return;
        };
        self.signin_waiters.push(reply);
        self.publish(|s| {
            s.signing_in = true;
            s.error = None;
        });
        let browser = self.deps.browser.clone();
        let tx = self.tx.clone();
        self.signin_task = Some(tokio::spawn(async move {
            let r = t.sign_in(&*browser).await;
            let _ = tx.send(Cmd::SignedIn(r));
        }));
    }

    fn cancel_sign_in(&mut self) {
        if let Some(t) = self.signin_task.take() {
            t.abort();
            for w in self.signin_waiters.drain(..) {
                let _ = w.send(Err("Sign-in cancelled".into()));
            }
            self.publish(|s| s.signing_in = false);
        }
    }

    async fn signed_in(&mut self, r: Result<TetherUser, Error>) {
        if self.signin_task.take().is_none() {
            return; // cancelled meanwhile
        }
        match r {
            Ok(user) => {
                self.remember_me(Some(user));
                self.publish(|s| {
                    s.signing_in = false;
                    s.signed_in = true;
                    s.error = None;
                });
                self.connect().await;
                let snap = self.snap.read().unwrap().clone();
                for w in self.signin_waiters.drain(..) {
                    let _ = w.send(Ok(snap.clone()));
                }
            }
            Err(e) => {
                let msg = e.to_string();
                self.publish(|s| {
                    s.signing_in = false;
                    s.error = Some(msg.clone());
                });
                for w in self.signin_waiters.drain(..) {
                    let _ = w.send(Err(msg.clone()));
                }
            }
        }
    }

    async fn sign_out(&mut self) -> Result<Snapshot, String> {
        self.cancel_sign_in();
        let gateway = {
            if let Some(p) = self.pump.take() {
                p.abort();
            }
            self.generation += 1;
            self.gateway.take()
        };
        let mut note = None;
        if let Some(t) = self.tether.clone() {
            if let Err(e) = t.sign_out(gateway).await {
                note = Some(e.to_string());
            }
        } else if let Some(g) = gateway {
            g.shutdown().await;
        }
        let _ = self.deps.db.set_setting(ME_KEY, &Option::<UserDto>::None);
        self.me = None;
        self.forget_people();
        self.forget_invites();
        self.publish(|s| {
            s.signed_in = false;
            s.signing_in = false;
            s.connection = "off";
            s.me = None;
            s.friends.clear();
            s.error = note;
        });
        Ok(self.snap.read().unwrap().clone())
    }

    async fn set_instance(&mut self, url: &str) -> Result<Snapshot, String> {
        let config = TetherConfig::new(url).map_err(|e| e.to_string())?;
        if config == self.config {
            return Ok(self.snap.read().unwrap().clone());
        }
        self.cancel_sign_in();
        self.deps.db.set_setting(INSTANCE_SETTING_KEY, &config.origin().to_string()).map_err(|e| e.to_string())?;
        self.config = config;
        self.me = None;
        self.forget_people();
        self.forget_invites();
        self.publish(|s| {
            s.me = None;
            s.signed_in = false;
            s.error = None;
        });
        self.rebuild_tether().await;
        Ok(self.snap.read().unwrap().clone())
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The invites of the last day, newest first, as tether's list and events give them.
#[derive(Debug, Default)]
pub struct InviteBook {
    list: Vec<GameInvite>,
}

/// tether lists at most 100.
const BOOK_MAX: usize = 100;

impl InviteBook {
    /// tether's list (after a READY).
    pub fn replace(&mut self, list: Vec<GameInvite>) {
        self.list = list;
        self.sort();
    }

    /// A created or changed invite (an event or a REST answer): replaces the copy with its id.
    pub fn upsert(&mut self, invite: GameInvite) {
        match self.list.iter_mut().find(|i| i.id == invite.id) {
            Some(old) => *old = invite,
            None => self.list.push(invite),
        }
        self.sort();
    }

    fn sort(&mut self) {
        self.list.sort_by_key(|i| std::cmp::Reverse((i.created_ms().unwrap_or(0), i.id.len(), i.id.clone())));
        self.list.truncate(BOOK_MAX);
    }

    /// When the next open invite runs out.
    pub fn next_expiry(&self, now: i64) -> Option<i64> {
        self.list.iter().filter(|i| i.is_open(now)).filter_map(GameInvite::expires_ms).min()
    }

    pub fn dtos(&self, me: &str, now: i64, person: &dyn Fn(&str) -> Person, library: &[LibGame]) -> Vec<InviteDto> {
        self.list.iter().filter(|i| i.sender_id == me || i.recipient_id == me).map(|i| invite_dto(i, me, now, person, library)).collect()
    }
}

/// One invite for the UI, its state on grapple's clock, and for an invite to Josh what Play does here.
pub fn invite_dto(i: &GameInvite, me: &str, now: i64, person: &dyn Fn(&str) -> Person, library: &[LibGame]) -> InviteDto {
    let outgoing = i.sender_id == me;
    let action = decide(&i.game.id, &i.game.name, library);
    InviteDto {
        id: i.id.clone(),
        outgoing,
        friend: person(if outgoing { &i.recipient_id } else { &i.sender_id }),
        game: InviteGameDto { id: i.game.id.clone(), name: i.game.name.clone() },
        state: i.state_at(now).as_str().to_string(),
        created_at: i.created_ms().unwrap_or(0),
        expires_at: i.expires_ms().unwrap_or(0),
        answered_at: i.answered_ms(),
        play: (!outgoing).then(|| action.kind()),
        library_game: action.library_game().map(String::from),
    }
}

fn a_origin(c: &TetherConfig) -> String {
    c.origin().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nowplaying::Playing;
    use grapple_core::{Art, Game, GameId, Store};
    use grapple_tether::{FriendGame, MemoryStore};
    use std::sync::Mutex;

    fn user(id: &str, name: &str) -> TetherUser {
        TetherUser { id: id.into(), username: name.into(), global_name: None, avatar: None }
    }

    fn lib() -> Vec<LibraryEntry> {
        vec![LibraryEntry { id: "steam:730".into(), key: "counter strike 2".into() }]
    }

    #[test]
    fn friends_map_to_the_library_and_offline_ones_are_not_online() {
        let playing = FriendPresence {
            user: TetherUser { global_name: Some("Rowan".into()), ..user("42", "rowan") },
            status: Status::Online,
            game: Some(FriendGame { name: "Counter-Strike 2".into(), since_ms: 1_700_000_123_000, image: Some(GameImage::Steam("730".into())) }),
        };
        let none = AvatarCache::new(std::env::temp_dir().join("grapple-unused-avatars"));
        let d = friend_dto(&playing, &lib(), &none);
        assert_eq!((d.name.as_str(), d.initial.as_str(), d.online, d.status), ("Rowan", "R", true, "online"));
        let p = d.playing.unwrap();
        assert_eq!((p.game.as_str(), p.since, p.owned), ("steam:730", 1_700_000_123, true));
        assert!(d.colour.starts_with("var(--avatar-"));

        let not_owned = FriendPresence {
            user: user("43", "kai"),
            status: Status::Idle,
            game: Some(FriendGame { name: "Hades II".into(), since_ms: 5_000, image: Some(GameImage::Steam("1145350".into())) }),
        };
        let p = friend_dto(&not_owned, &lib(), &none).playing.unwrap();
        assert_eq!((p.game.as_str(), p.owned), ("tether:steam:1145350", false));

        let off = FriendPresence { user: user("44", "mira"), status: Status::Offline, game: None };
        let d = friend_dto(&off, &lib(), &none);
        assert_eq!((d.online, d.status, d.playing), (false, "offline", None));
    }

    const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 body";

    #[test]
    fn the_picture_url_is_set_only_once_the_file_is_cached_for_that_hash() {
        let dir = std::env::temp_dir().join(format!("grapple-svc-avatar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = AvatarCache::new(dir.clone());
        let with_pic = |hash: Option<&str>| TetherUser { avatar: hash.map(Into::into), ..user("42", "rowan") };
        let presence = |u: TetherUser| FriendPresence { user: u, status: Status::Online, game: None };

        // has a picture on tether, not downloaded yet: the initial is drawn
        assert_eq!(user_dto(&with_pic(Some("h1")), &cache).avatar, None);
        assert_eq!(friend_dto(&presence(with_pic(Some("h1"))), &lib(), &cache).avatar, None);

        // downloaded and marked ready: the URL carries the id and the hash
        cache.store("42", "h1", grapple_tether::AvatarImage { kind: grapple_tether::ImageKind::Webp, bytes: WEBP.to_vec() }).unwrap();
        assert!(cache.mark_ready("42", "h1"));
        let me = user_dto(&with_pic(Some("h1")), &cache);
        let url = me.avatar.clone().expect("cached");
        assert!(url.contains("avatar/42") && url.ends_with("?v=h1"), "{url}");
        assert_eq!(friend_dto(&presence(with_pic(Some("h1"))), &lib(), &cache).avatar.as_deref(), Some(url.as_str()));
        // the same person with a new hash is not served the old file; one without a picture has none
        assert_eq!(user_dto(&with_pic(Some("h2")), &cache).avatar, None);
        assert_eq!(user_dto(&with_pic(None), &cache).avatar, None);
        // somebody else's id never gets this file
        assert_eq!(user_dto(&TetherUser { avatar: Some("h1".into()), ..user("43", "kai") }, &cache).avatar, None);
        // the colour and initial stay for the fallback
        assert!(me.colour.starts_with("var(--avatar-") && me.initial == "R");
        // an old saved `tether.me` without the field still reads
        let old: UserDto = serde_json::from_str(r#"{"id":"1","name":"j","initial":"J","colour":"var(--avatar-0)"}"#).unwrap();
        assert_eq!(old.avatar, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A local stand-in for tether's avatar route (never a real instance): answers `/api/v1/avatars/<id>/<hash>`
    /// with a tiny WebP and counts the requests.
    async fn avatar_server() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", l.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let mut buf = vec![0u8; 4096];
                let n = c.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let head = if req.starts_with("GET /api/v1/avatars/") {
                    format!("HTTP/1.1 200 OK\r\ncontent-type: image/webp\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", WEBP.len())
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into()
                };
                let _ = c.write_all(head.as_bytes()).await;
                if req.starts_with("GET /api/v1/avatars/") {
                    let _ = c.write_all(WEBP).await;
                }
            }
        });
        (origin, hits)
    }

    #[tokio::test]
    async fn a_picture_downloads_in_the_background_then_the_snapshot_gets_its_url() {
        let (origin, hits) = avatar_server().await;
        let dir = std::env::temp_dir().join(format!("grapple-svc-avatar-flow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let snaps: Arc<Mutex<Vec<Snapshot>>> = Arc::default();
        let sink = snaps.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let snap = Arc::new(RwLock::new(Snapshot { connection: "off", ..Snapshot::default() }));
        let avatars = Arc::new(AvatarCache::new(dir.join("avatars")));
        let deps = Deps {
            db: Db::open_in_memory().unwrap(),
            make_tether: Arc::new(|c| Tether::new(c, Arc::new(MemoryStore::new()))),
            browser: Arc::new(NoBrowser),
            emit: Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
            icon_dir: dir.join("icons"),
            avatar_dir: dir.join("avatars"),
            desktop_settings: None,
            emit_invites: Arc::new(|_| {}),
            play: Arc::new(|_| {}),
        };
        let mut a = Actor::new(deps, tx, snap.clone(), avatars.clone());
        a.tether = Some(Arc::new(Tether::new(TetherConfig::new(&origin).unwrap(), Arc::new(MemoryStore::new())).unwrap()));
        let josh = |hash: Option<&str>| TetherUser { avatar: hash.map(Into::into), ..user("42", "josh") };

        // signed in with a picture: the snapshot shows the initial now, nothing waits for the download
        a.remember_me(Some(josh(Some("h1"))));
        assert_eq!(snap.read().unwrap().me.as_ref().unwrap().avatar, None);
        assert_eq!(avatars.file("42"), None);
        let Cmd::Avatar(id, hash, r) = next_cmd(&mut rx).await else { panic!("a download result") };
        assert_eq!((id.as_str(), hash.as_str(), r.clone()), ("42", "h1", Ok(())));
        let before = snaps.lock().unwrap().len();
        a.avatar_done(id, hash, r);
        let me = snap.read().unwrap().me.clone().unwrap();
        assert!(me.avatar.as_deref().is_some_and(|u| u.contains("avatar/42") && u.ends_with("?v=h1")), "{:?}", me.avatar);
        assert!(snaps.lock().unwrap().len() > before, "tether-changed is emitted when the picture lands");
        assert_eq!(avatars.file("42").unwrap().hash, "h1");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

        // the same picture again (presence, library change): no new request
        a.remember_me(Some(josh(Some("h1"))));
        a.refresh_friends();
        assert!(snap.read().unwrap().me.as_ref().unwrap().avatar.is_some());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err(), "nothing to download");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);

        // a new picture: the old URL goes at once, the new file replaces the old one
        a.remember_me(Some(josh(Some("h2"))));
        assert_eq!(snap.read().unwrap().me.as_ref().unwrap().avatar, None);
        let Cmd::Avatar(id, hash, r) = next_cmd(&mut rx).await else { panic!("a download result") };
        a.avatar_done(id, hash, r);
        assert!(snap.read().unwrap().me.as_ref().unwrap().avatar.as_deref().unwrap().ends_with("?v=h2"));
        let files: Vec<_> = std::fs::read_dir(dir.join("avatars")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(files, vec!["42-h2.webp".to_string()]);

        // signing out: nobody is served any more
        a.forget_people();
        assert_eq!(avatars.file("42"), None);

        // a result for somebody who left the snapshot while it downloaded is not served
        a.remember_me(Some(josh(Some("h3"))));
        let Cmd::Avatar(id, hash, r) = next_cmd(&mut rx).await else { panic!("a download result") };
        a.forget_people();
        a.avatar_done(id, hash, r);
        assert_eq!(avatars.file("42"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -------------------------------------------------------------------------- invites

    fn invite(id: &str, sender: &str, recipient: &str, game: (&str, &str), state: &str, created: &str, expires: &str) -> GameInvite {
        serde_json::from_value(serde_json::json!({
            "id": id, "channel_id": "9", "message_id": id, "sender_id": sender, "recipient_id": recipient,
            "game": { "id": game.0, "name": game.1, "image": null }, "state": state, "joinable": false,
            "created_at": created, "expires_at": expires, "seen_at": null, "answered_at": null
        }))
        .unwrap()
    }

    fn lib_games() -> Vec<LibGame> {
        vec![
            LibGame::of(&game(Store::Steam, "730", "Counter-Strike 2", None)),
            LibGame::of(&Game { installed: false, ..game(Store::Epic, "Fall", "Fall Guys", None) }),
        ]
    }

    fn someone(id: &str) -> Person {
        Person { id: id.into(), name: format!("user {id}"), initial: "U".into(), colour: "var(--avatar-1)".into(), avatar: None }
    }

    const T0: &str = "2026-11-25T18:00:00.000Z";
    const T1: &str = "2026-11-25T18:01:00.000Z";
    const END: &str = "2026-11-25T18:15:00.000Z";

    #[test]
    fn the_invite_list_follows_events_and_expires_on_grapples_clock() {
        let at = |s: &str| grapple_tether::parse_iso_ms(s).unwrap();
        let mut book = InviteBook::default();
        book.replace(vec![
            invite("1", "100", "200", ("steam:730", "Counter-Strike 2"), "sent", T0, END),
            invite("2", "300", "100", ("epic:Fall", "Fall Guys"), "sent", T1, END),
        ]);
        let dtos = |b: &InviteBook, now: i64| b.dtos("100", now, &someone, &lib_games());
        let d = dtos(&book, at(T1));
        // Newest first; the friend is the other person; what Play does is said only for invites to Josh.
        assert_eq!(d.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["2", "1"]);
        assert_eq!((d[1].outgoing, d[1].friend.id.as_str(), d[1].play), (true, "200", None));
        assert_eq!((d[0].outgoing, d[0].friend.id.as_str(), d[0].play, d[0].library_game.as_deref()), (false, "300", Some("install"), Some("epic:Fall")));
        assert_eq!((d[1].created_at, d[1].expires_at), (at(T0), at(END)));
        assert_eq!(book.next_expiry(at(T1)), Some(at(END)));

        // GAME_INVITE_UPDATE replaces the copy by id; GAME_INVITE_CREATE adds one.
        book.upsert(invite("1", "100", "200", ("steam:730", "Counter-Strike 2"), "accepted", T0, END));
        book.upsert(invite("3", "100", "300", ("manual:x", "Doom"), "seen", "2026-11-25T18:02:00.000Z", END));
        let d = dtos(&book, at(T1));
        assert_eq!(d.iter().map(|x| (x.id.as_str(), x.state.as_str())).collect::<Vec<_>>(), [("3", "seen"), ("2", "sent"), ("1", "accepted")]);

        // At expires_at the open ones show as expired before tether's sweep says so; accepted stays.
        let d = dtos(&book, at(END));
        assert_eq!(d.iter().map(|x| x.state.as_str()).collect::<Vec<_>>(), ["expired", "expired", "accepted"]);
        assert_eq!(book.next_expiry(at(END)), None);
        // Someone else's invite (it should never come) is not listed; the list stays at tether's 100.
        book.upsert(invite("4", "200", "300", ("steam:730", "x"), "sent", T1, END));
        assert_eq!(dtos(&book, at(T1)).len(), 3);
        for i in 0..150 {
            book.upsert(invite(&format!("{}", 1000 + i), "100", "200", ("steam:730", "x"), "sent", T1, END));
        }
        assert_eq!(book.list.len(), BOOK_MAX);
    }

    #[tokio::test]
    async fn gateway_invite_events_reach_the_ui_and_play_reaches_the_app() {
        let snaps: Arc<Mutex<Vec<InvitesSnapshot>>> = Arc::default();
        let plays: Arc<Mutex<Vec<PlayRequest>>> = Arc::default();
        let (sink, played) = (snaps.clone(), plays.clone());
        let (tx, _rx) = mpsc::unbounded_channel();
        let deps = Deps {
            db: Db::open_in_memory().unwrap(),
            make_tether: Arc::new(|c| Tether::new(c, Arc::new(MemoryStore::new()))),
            browser: Arc::new(NoBrowser),
            emit: Arc::new(|_| {}),
            icon_dir: std::env::temp_dir().join("grapple-unused-icons"),
            avatar_dir: std::env::temp_dir().join("grapple-unused-avatars"),
            desktop_settings: None,
            emit_invites: Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
            play: Arc::new(move |r| played.lock().unwrap().push(r)),
        };
        let mut a = Actor::new(deps, tx, Arc::default(), Arc::new(AvatarCache::new(std::env::temp_dir().join("grapple-unused-avatars"))));
        a.me = Some(UserDto { id: "100".into(), name: "josh".into(), initial: "J".into(), colour: "var(--avatar-0)".into(), avatar: None });
        a.lib_games = lib_games();
        let last = || snaps.lock().unwrap().last().cloned().unwrap();

        // Signed out: nothing is listed.
        a.on_event(TetherEvent::InviteCreated(invite("1", "100", "200", ("steam:730", "Counter-Strike 2"), "sent", T0, "2099-01-01T00:00:00.000Z"))).await;
        assert_eq!((last().status, last().invites.len()), ("off", 0));
        a.invites_status = "ready";
        a.on_event(TetherEvent::InviteUpdated(invite("1", "100", "200", ("steam:730", "Counter-Strike 2"), "accepted", T0, "2099-01-01T00:00:00.000Z"))).await;
        let s = last();
        assert_eq!((s.status, s.invites.len(), s.invites[0].state.as_str()), ("ready", 1, "accepted"));
        assert_eq!(s.invites[0].friend.name, "Someone", "not a friend in this session: a plain stand-in");

        // Play pressed in tether: listed as accepted, and handed to the app with its payload.
        let played = invite("2", "300", "100", ("steam:730", "Counter-Strike 2"), "accepted", T1, "2099-01-01T00:00:00.000Z");
        let join = grapple_tether::InviteJoin::Text(grapple_tether::Secret::new("pw"));
        a.on_event(TetherEvent::InvitePlay { invite: played.clone(), join: Some(join.clone()) }).await;
        assert_eq!(plays.lock().unwrap().as_slice(), [PlayRequest { game: played.game.clone(), join: Some(join) }]);
        assert_eq!(last().invites[0].play, Some("launch"));
        // Never in the snapshot.
        assert!(!serde_json::to_string(&last()).unwrap().contains("\"pw\""));

        // A deep link before any invite loaded: the view waits (no fetch while invites are not ready).
        a.invites_status = "off";
        a.open = Some(OpenState { id: "77".into(), ..OpenState::default() });
        a.fetch_open();
        a.publish_invites();
        assert_eq!(last().open, Some(OpenInvite { id: "77".into(), invite: None, error: None }));
        a.forget_invites();
        assert_eq!(last().open.map(|o| o.id), Some("77".into()), "signing out keeps the link asked for");
    }

    async fn next_cmd(rx: &mut mpsc::UnboundedReceiver<Cmd>) -> Cmd {
        tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("a message in time").expect("open channel")
    }

    #[test]
    fn initials_skip_symbols() {
        assert_eq!(initial_of("josh"), "J");
        assert_eq!(initial_of("_x"), "X");
        assert_eq!(initial_of("!!"), "?");
    }

    #[test]
    fn prepare_icon_makes_caches_and_remakes_when_the_picture_changes() {
        let dir = std::env::temp_dir().join(format!("grapple-svc-icon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("cover.png");
        image::RgbaImage::from_pixel(80, 120, image::Rgba([200, 30, 30, 255])).save(&src).unwrap();
        let db = Db::open_in_memory().unwrap();
        let id = GameId("epic:Sugar".into());
        let cache = dir.join("cache");

        let missing = dir.join("nope.png");
        let a = prepare_icon(&db, &id, &[missing, src.clone()], &cache).expect("falls through to the next file");
        let rec: IconRecord = db.setting(&icon_key(&id)).unwrap().unwrap();
        assert_eq!(rec.sha, sha256_hex(&a));
        assert!(cache.join(format!("{}.png", rec.sha)).exists());

        // second call is served from the cache (same bytes), even if the source file is now unreadable as an image
        let b = prepare_icon(&db, &id, &[src.clone()], &cache).unwrap();
        assert_eq!(a, b);

        // a changed picture (new mtime) is made again
        std::thread::sleep(Duration::from_millis(1100));
        image::RgbaImage::from_pixel(80, 120, image::Rgba([30, 30, 200, 255])).save(&src).unwrap();
        let c = prepare_icon(&db, &id, &[src.clone()], &cache).unwrap();
        assert_ne!(a, c);
        assert_eq!(db.setting::<IconRecord>(&icon_key(&id)).unwrap().unwrap().sha, sha256_hex(&c));

        // nothing usable -> None, no panic
        assert_eq!(prepare_icon(&db, &id, &[dir.join("none")], &cache), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    struct NoBrowser;
    impl Browser for NoBrowser {
        fn open(&self, _url: &url::Url) -> grapple_tether::Result<()> {
            Ok(()) // nobody answers: the sign-in just waits
        }
    }

    fn offline_service(db: Db) -> (TetherService, Arc<Mutex<Vec<Snapshot>>>) {
        let snaps: Arc<Mutex<Vec<Snapshot>>> = Arc::default();
        let sink = snaps.clone();
        let svc = TetherService::spawn(Deps {
            db,
            make_tether: Arc::new(|c| Tether::new(c, Arc::new(MemoryStore::new()))),
            browser: Arc::new(NoBrowser),
            emit: Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
            icon_dir: std::env::temp_dir().join("grapple-unused-icons"),
            avatar_dir: std::env::temp_dir().join("grapple-unused-avatars"),
            desktop_settings: None,
            emit_invites: Arc::new(|_| {}),
            play: Arc::new(|_| {}),
        });
        (svc, snaps)
    }

    #[test]
    fn the_desktop_apps_host_is_read_and_only_that() {
        let dir = std::env::temp_dir().join(format!("grapple-desktop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.json");
        let case = |json: &str| {
            std::fs::write(&f, json).unwrap();
            desktop_host(&f)
        };
        assert_eq!(case(r#"{"host":"https://chat.example"}"#).as_deref(), Some("https://chat.example"));
        assert_eq!(case(r#"{"host":{"url":"https://chat.example"}}"#).as_deref(), Some("https://chat.example"));
        // null host: knownHosts is ignored (it keeps old addresses)
        assert_eq!(case(r#"{"host":null,"knownHosts":["https://old.example"]}"#), None);
        assert_eq!(case(r#"{"knownHosts":["https://old.example"]}"#), None);
        assert_eq!(case("not json"), None);
        assert_eq!(desktop_host(&dir.join("missing.json")), None);

        // the order: Grapple's setting, the desktop app's host, the official tether
        std::fs::write(&f, r#"{"host":"https://chat.example"}"#).unwrap();
        assert_eq!(resolve_instance(Some("https://mine.example"), Some(&f)).origin(), "https://mine.example");
        assert_eq!(resolve_instance(None, Some(&f)).origin(), "https://chat.example");
        assert_eq!(resolve_instance(Some(" "), None).origin(), "https://app.tethr.lol");
        std::fs::write(&f, r#"{"host":null}"#).unwrap();
        assert_eq!(resolve_instance(None, Some(&f)).origin(), "https://app.tethr.lol");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_waiting_sign_in_can_be_cancelled_and_the_instance_setting_is_checked_and_saved() {
        let db = Db::open_in_memory().unwrap();
        let (svc, snaps) = offline_service(db.clone());
        svc.start();
        let s = wait_for(&snaps, "loaded", |s| s.loaded).await;
        assert_eq!((s.signed_in, s.connection, s.instance_url.as_str()), (false, "off", "https://app.tethr.lol"));

        // Never talk to the real tether from a test: a local stand-in whose authorize route exists (401),
        // so the pre-sign-in check passes and the sign-in waits for the browser.
        let local = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", l.local_addr().unwrap());
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while let Ok((mut c, _)) = l.accept().await {
                    let mut buf = [0u8; 2048];
                    let _ = c.read(&mut buf).await;
                    let _ = c.write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
                }
            });
            origin
        };
        svc.set_instance_url(local).await.unwrap();

        let waiting = tokio::spawn({
            let svc = svc.clone();
            async move { svc.sign_in().await }
        });
        wait_for(&snaps, "signing in", |s| s.signing_in).await;
        svc.cancel_sign_in();
        let r = waiting.await.unwrap();
        assert!(r.unwrap_err().contains("cancelled"));
        let s = wait_for(&snaps, "no longer signing in", |s| !s.signing_in).await;
        assert!(!s.signed_in && s.error.is_none());

        assert!(svc.set_instance_url("http://tether.example".into()).await.is_err(), "plain http is only for local and tailnet addresses");
        assert!(svc.set_instance_url("not a url".into()).await.is_err());
        let s = svc.set_instance_url("https://tether.example/".into()).await.unwrap();
        assert_eq!(s.instance_url, "https://tether.example");
        assert_eq!(db.setting::<String>(INSTANCE_SETTING_KEY).unwrap().as_deref(), Some("https://tether.example"));
        svc.shutdown(Duration::from_secs(1)).await;
    }

    // -------------------------------------------------------------------------- against a local dev:lite

    /// Answers the authorize request with a person's session, the way the web page's Allow does (the test
    /// stands in for the browser; nothing is ever opened).
    struct SessionBrowser {
        origin: String,
        session: String,
    }

    impl Browser for SessionBrowser {
        fn open(&self, url: &url::Url) -> grapple_tether::Result<()> {
            use serde_json::{Value, json};
            let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
            let (origin, session) = (self.origin.clone(), self.session.clone());
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                let mut check = url::Url::parse(&format!("{origin}/api/v1/oauth2/authorize")).unwrap();
                check.query_pairs_mut().extend_pairs(q.iter());
                let r = http.get(check).bearer_auth(&session).send().await.unwrap();
                assert_eq!(r.status(), 200);
                let mut body: serde_json::Map<String, Value> = q.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
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
                let res = http.get(answer["location"].as_str().unwrap()).send().await.unwrap();
                assert!(res.status().is_success());
            });
            Ok(())
        }
    }

    async fn login(origin: &str, name: &str, password: &str) -> String {
        let res: serde_json::Value = reqwest::Client::new()
            .post(format!("{origin}/api/v1/auth/login"))
            .json(&serde_json::json!({ "login": name, "password": password }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        res["token"].as_str().expect("logged in to the local dev server").to_string()
    }

    fn game(store: Store, id: &str, name: &str, icon: Option<PathBuf>) -> Game {
        Game {
            id: GameId::new(store, id),
            store,
            name: name.into(),
            sort_name: grapple_core::sort_name(name),
            installed: true,
            install_dir: None,
            size_bytes: None,
            art: Art { icon, ..Art::default() },
            store_playtime_min: 0,
            last_played: None,
            status: None,
        }
    }

    async fn wait_for(snaps: &Arc<Mutex<Vec<Snapshot>>>, what: &str, pred: impl Fn(&Snapshot) -> bool) -> Snapshot {
        for _ in 0..200 {
            if let Some(s) = snaps.lock().unwrap().iter().rev().find(|s| pred(s)).cloned() {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for: {what}; last = {:?}", snaps.lock().unwrap().last());
    }

    /// The whole app-side flow against a LOCAL tether (`pnpm dev:lite --seed`, never a real instance):
    ///   GRAPPLE_TETHER_DEV_URL=http://localhost:5311 GRAPPLE_TETHER_DEV_PASSWORD=password \
    ///     nice -n 19 cargo test -p grapple --features real -- --ignored dev_lite
    /// Signs in as `josh` through the authorize API, connects, publishes a Steam and an Epic game (uploading
    /// an icon), checks that rowan's own client sees them and that josh sees rowan's presence mapped to the
    /// library, then signs out.
    #[tokio::test]
    #[ignore = "needs a local dev:lite from the grapple-apps branch"]
    async fn dev_lite_end_to_end() {
        let origin = std::env::var("GRAPPLE_TETHER_DEV_URL").expect("GRAPPLE_TETHER_DEV_URL");
        let password = std::env::var("GRAPPLE_TETHER_DEV_PASSWORD").expect("GRAPPLE_TETHER_DEV_PASSWORD");
        let cfg = TetherConfig::new(&origin).unwrap();
        assert!(cfg.origin().starts_with("http://localhost") || cfg.origin().starts_with("http://127.0.0.1"), "local dev servers only");

        let dir = std::env::temp_dir().join(format!("grapple-devlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let icon = dir.join("icon.png");
        image::RgbaImage::from_pixel(256, 256, image::Rgba([74, 222, 128, 255])).save(&icon).unwrap();

        // rowan's side: a plain grapple-tether client, to see what josh publishes and to play something.
        let rowan_session = login(&origin, "rowan", &password).await;
        let rowan = Tether::new(cfg.clone(), Arc::new(MemoryStore::new())).unwrap();
        rowan.sign_in(&SessionBrowser { origin: origin.clone(), session: rowan_session }).await.unwrap();
        let rowan_gw = rowan.connect();
        let rowan_rx = rowan_gw.subscribe();

        let db = Db::open_in_memory().unwrap();
        let snaps: Arc<Mutex<Vec<Snapshot>>> = Arc::default();
        let sink = snaps.clone();
        let josh_session = login(&origin, "josh", &password).await;
        let svc = TetherService::spawn(Deps {
            db: db.clone(),
            make_tether: Arc::new(|c| Tether::new(c, Arc::new(MemoryStore::new()))),
            browser: Arc::new(SessionBrowser { origin: origin.clone(), session: josh_session }),
            emit: Arc::new(move |s| sink.lock().unwrap().push(s.clone())),
            icon_dir: dir.join("icons"),
            avatar_dir: dir.join("avatars"),
            desktop_settings: None,
            emit_invites: Arc::new(|_| {}),
            play: Arc::new(|_| {}),
        });
        // point the service at the local instance the way the setting does
        svc.start();
        wait_for(&snaps, "loaded", |s| s.loaded).await;
        let s = svc.set_instance_url(origin.clone()).await.unwrap();
        assert_eq!(s.instance_url, origin);
        assert!(!s.signed_in);

        let library = vec![
            game(Store::Steam, "730", "Counter-Strike 2", None),
            game(Store::Epic, "Sugar", "Rocket League", Some(icon.clone())),
        ];
        svc.set_library(&library);
        // a game started before the sign-in is sent as soon as the connection is up
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 - 120;
        svc.set_playing(Some(Playing { game: GameId("steam:730".into()), name: "Counter-Strike 2".into(), started_at: started, art: ArtSource::Steam(730) }));

        let rowan_sees = |name: &'static str| {
            let mut rx = rowan_rx.resubscribe();
            async move {
                tokio::time::timeout(Duration::from_secs(10), async move {
                    loop {
                        if let TetherEvent::Friend(p) = rx.recv().await.unwrap()
                            && p.user.username == "josh"
                            && p.game.as_ref().is_some_and(|g| g.name == name)
                        {
                            return p;
                        }
                    }
                })
                .await
                .expect("rowan sees josh's game")
            }
        };
        let first = rowan_sees("Counter-Strike 2");
        let s = svc.sign_in().await.unwrap();
        assert!(s.signed_in);
        let online = wait_for(&snaps, "online", |s| s.connection == "online" && s.me.is_some()).await;
        assert_eq!(online.me.as_ref().unwrap().name, "josh");
        assert!(online.friends.iter().any(|f| f.name == "rowan"), "josh's friends include rowan");

        let first = first.await;
        assert!(matches!(first.game.unwrap().image, Some(GameImage::Steam(ref id)) if id == "730"));

        // an Epic game: made into an icon, uploaded, shown as icon:<sha>
        let epic = rowan_sees("Rocket League");
        svc.set_playing(Some(Playing {
            game: GameId("epic:Sugar".into()),
            name: "Rocket League".into(),
            started_at: started,
            art: ArtSource::Files(vec![icon.clone()]),
        }));
        let p = epic.await;
        let image = p.game.unwrap().image.expect("an uploaded icon");
        assert!(matches!(image, GameImage::Icon(_)));
        assert!(rowan.api().game_icon(&image).await.unwrap().is_some(), "rowan can fetch the uploaded icon");
        let rec: IconRecord = db.setting(&icon_key(&GameId("epic:Sugar".into()))).unwrap().unwrap();
        assert!(matches!(&image, GameImage::Icon(sha) if *sha == rec.sha), "the cached sha is the one tether has");

        // rowan plays a game josh owns: josh's snapshot maps it onto the library
        rowan_gw.set_game(Some(GameActivity::steam("Counter-Strike 2", 730, started * 1000))).await.unwrap();
        let s = wait_for(&snaps, "rowan in a library game", |s| {
            s.friends.iter().any(|f| f.name == "rowan" && f.playing.as_ref().is_some_and(|p| p.game == "steam:730"))
        })
        .await;
        assert!(s.friends.iter().find(|f| f.name == "rowan").unwrap().playing.as_ref().unwrap().owned);

        // clearing it clears it for friends
        let gone = {
            let mut rx = rowan_rx.resubscribe();
            tokio::time::timeout(Duration::from_secs(10), async move {
                loop {
                    // Clearing shows josh's desktop-detected game again if the seed scene gave him one.
                    if let TetherEvent::Friend(p) = rx.recv().await.unwrap()
                        && p.user.username == "josh"
                        && p.game.as_ref().is_none_or(|g| g.name != "Rocket League")
                    {
                        return;
                    }
                }
            })
        };
        svc.set_playing(None);
        gone.await.expect("rowan sees josh stop playing");

        let out = svc.sign_out().await.unwrap();
        assert!(!out.signed_in && out.friends.is_empty() && out.me.is_none());
        svc.shutdown(Duration::from_secs(3)).await;
        rowan.sign_out(Some(rowan_gw)).await.unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
