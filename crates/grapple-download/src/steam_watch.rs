//! Following Steam's downloads from its files (no bridge): `appmanifest_<id>.acf` (StateFlags and byte counters),
//! the size of `steamapps/downloading/<id>/`, and new lines of `logs/content_log.txt`. Read-only.

use crate::fsutil;
use grapple_scan::steam_flags as flags;
use grapple_scan::vdf::Vdf;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What one `appmanifest_<id>.acf` says.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Acf {
    pub appid: u32,
    pub name: String,
    pub state_flags: u64,
    pub installdir: String,
    pub size_on_disk: u64,
    pub buildid: u64,
    pub target_buildid: u64,
    pub bytes_to_download: u64,
    pub bytes_downloaded: u64,
    pub bytes_to_stage: u64,
    pub bytes_staged: u64,
    /// The library folder the acf is in.
    pub library: PathBuf,
}

pub fn parse_acf(text: &str, library: &Path) -> Option<Acf> {
    let v = Vdf::parse(text);
    let st = v.get("AppState")?;
    let n = |k: &str| st.u64_at(&[k]).unwrap_or(0);
    Some(Acf {
        appid: st.u64_at(&["appid"])? as u32,
        name: st.str_at(&["name"]).unwrap_or_default().to_string(),
        state_flags: n("StateFlags"),
        installdir: st.str_at(&["installdir"]).unwrap_or_default().to_string(),
        size_on_disk: n("SizeOnDisk"),
        buildid: n("buildid"),
        target_buildid: n("TargetBuildID"),
        bytes_to_download: n("BytesToDownload"),
        bytes_downloaded: n("BytesDownloaded"),
        bytes_to_stage: n("BytesToStage"),
        bytes_staged: n("BytesStaged"),
        library: library.to_path_buf(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamPhase {
    /// Waiting in Steam's queue (or an update Steam will do on its own schedule).
    Queued,
    Downloading,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamKind {
    Install,
    Update,
}

/// One Steam download as grapple shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SteamApp {
    pub appid: u32,
    pub name: String,
    pub kind: SteamKind,
    pub phase: SteamPhase,
    pub done: u64,
    pub total: Option<u64>,
    pub bytes_per_sec: Option<u64>,
    pub seconds_left: Option<u64>,
    /// Steam's queue position (bridge), else the order grapple first saw it.
    pub queue_index: Option<u32>,
    pub library: PathBuf,
    /// Installed and needing an update, nothing running (the "Updates" list).
    pub update_available: bool,
}

/// What a manifest's flags and counters mean for grapple. `None`: nothing to show (installed, up to date).
pub fn classify(a: &Acf, downloading_dir_size: u64) -> Option<SteamApp> {
    let f = a.state_flags;
    let installed = f & flags::FULLY_INSTALLED != 0;
    let active = f & flags::ACTIVE_UPDATE != 0;
    let paused = f & flags::UPDATE_PAUSED != 0;
    let required = f & flags::UPDATE_REQUIRED != 0;
    if !(active || paused || required) {
        return None;
    }
    let kind = if installed { SteamKind::Update } else { SteamKind::Install };
    let phase = if paused {
        SteamPhase::Paused
    } else if active {
        SteamPhase::Downloading
    } else {
        SteamPhase::Queued
    };
    let total = (a.bytes_to_download > 0).then_some(a.bytes_to_download);
    let mut done = a.bytes_downloaded;
    // Steam writes the acf now and then; the staging folder grows live. Scale its size from staged bytes to
    // download bytes (downloads are compressed).
    if let Some(t) = total
        && a.bytes_to_stage > 0
        && downloading_dir_size > 0
    {
        let est = (downloading_dir_size as u128 * t as u128 / a.bytes_to_stage as u128) as u64;
        done = done.max(est.min(t));
    }
    Some(SteamApp {
        appid: a.appid,
        name: a.name.clone(),
        kind,
        phase,
        done,
        total,
        bytes_per_sec: None,
        seconds_left: None,
        queue_index: None,
        library: a.library.clone(),
        update_available: installed && phase == SteamPhase::Queued,
    })
}

/// One parsed `content_log.txt` line: `[2026-10-19 14:50:09] AppID 5246870 <what>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    /// `state changed : Update Required,Update Queued,`
    State { appid: u32, states: Vec<String> },
    /// `App update changed : Running Update,Downloading,Staging,` (`None` when it ends)
    Update { appid: u32, states: Vec<String> },
    /// `update started : download 0/394633216, store 0/0, reuse 0/0, delta 0/0, stage 0/1104001082`
    Started { appid: u32, download: (u64, u64), stage: (u64, u64) },
    /// `finished update, 1 mounted depots (BuildID 26041765) : ...`
    Finished { appid: u32, buildid: Option<u64> },
}

fn list(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|x| !x.is_empty()).map(str::to_string).collect()
}

fn pair(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.trim().split_once('/')?;
    Some((a.trim().parse().ok()?, b.trim().trim_end_matches(',').parse().ok()?))
}

pub fn parse_log_line(line: &str) -> Option<LogEvent> {
    let rest = line.trim_end().split_once("] ")?.1;
    let rest = rest.strip_prefix("AppID ")?;
    let (id, what) = rest.split_once(' ')?;
    let appid: u32 = id.parse().ok()?;
    if let Some(s) = what.strip_prefix("state changed : ") {
        return Some(LogEvent::State { appid, states: list(s) });
    }
    if let Some(s) = what.strip_prefix("App update changed : ") {
        let states = list(s).into_iter().filter(|x| x != "None").collect();
        return Some(LogEvent::Update { appid, states });
    }
    if let Some(s) = what.strip_prefix("update started : ") {
        let mut download = None;
        let mut stage = None;
        for part in s.split(", ") {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("download ") {
                download = pair(v);
            } else if let Some(v) = part.strip_prefix("stage ") {
                stage = pair(v);
            }
        }
        return Some(LogEvent::Started { appid, download: download?, stage: stage.unwrap_or((0, 0)) });
    }
    if what.starts_with("finished update") {
        let buildid = what.split_once("(BuildID ").and_then(|(_, r)| r.split(')').next()).and_then(|b| b.trim().parse().ok());
        return Some(LogEvent::Finished { appid, buildid });
    }
    None
}

/// Reads only the lines added since the last call; starts over when the file got shorter (Steam rotated it).
pub struct LogTail {
    path: PathBuf,
    offset: u64,
}

impl LogTail {
    /// `from_start`: read what is already there too (tests); the app starts at the end.
    pub fn new(path: PathBuf, from_start: bool) -> LogTail {
        let offset = if from_start { 0 } else { std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) };
        LogTail { path, offset }
    }

    pub fn read_new(&mut self) -> Vec<LogEvent> {
        let Ok(mut f) = std::fs::File::open(&self.path) else { return Vec::new() };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        // At most 4 MiB per read; the rest comes next time.
        let mut buf = Vec::new();
        let _ = f.take(4 << 20).read_to_end(&mut buf);
        // Only whole lines.
        let end = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
        self.offset += end as u64;
        String::from_utf8_lossy(&buf[..end]).lines().filter_map(parse_log_line).collect()
    }
}

/// The watcher: one `poll` reads every library's manifests and the log, and says what is downloading, how fast,
/// and which downloads finished since the last poll.
pub struct SteamWatch {
    root: PathBuf,
    libraries: Vec<PathBuf>,
    log: LogTail,
    /// appid → (done bytes, when) of the last poll, for the speed.
    last: HashMap<u32, (u64, Instant)>,
    /// Totals from the log when the acf has none yet.
    log_totals: HashMap<u32, u64>,
    /// The order downloads first showed up (Steam's own order is only known with the bridge).
    seen: HashMap<u32, u32>,
    next_seen: u32,
}

#[derive(Debug, Default)]
pub struct WatchResult {
    pub apps: Vec<SteamApp>,
    /// Downloads that were seen running and are now installed.
    pub finished: Vec<u32>,
}

impl SteamWatch {
    pub fn new(root: PathBuf, log_from_start: bool) -> SteamWatch {
        let libraries = grapple_scan::steam_library_dirs(&root);
        let log = LogTail::new(root.join("logs").join("content_log.txt"), log_from_start);
        SteamWatch { root, libraries, log, last: HashMap::new(), log_totals: HashMap::new(), seen: HashMap::new(), next_seen: 0 }
    }

    pub fn libraries(&self) -> &[PathBuf] {
        &self.libraries
    }

    pub fn poll(&mut self) -> WatchResult {
        self.libraries = grapple_scan::steam_library_dirs(&self.root);
        let mut finished = Vec::new();
        for ev in self.log.read_new() {
            match ev {
                LogEvent::Started { appid, download, .. } if download.1 > 0 => {
                    self.log_totals.insert(appid, download.1);
                }
                LogEvent::Finished { appid, .. }
                    if self.last.contains_key(&appid) => {
                        finished.push(appid);
                    }
                _ => {}
            }
        }
        let now = Instant::now();
        let mut apps = Vec::new();
        let mut installed_now: HashMap<u32, bool> = HashMap::new();
        for lib in &self.libraries {
            let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
            for e in rd.flatten() {
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if !(name.starts_with("appmanifest_") && name.ends_with(".acf")) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
                let Some(acf) = parse_acf(&text, lib) else { continue };
                installed_now.insert(acf.appid, acf.state_flags & flags::FULLY_INSTALLED != 0 && acf.state_flags & flags::UPDATE_REQUIRED == 0);
                let dl = lib.join("steamapps").join("downloading").join(acf.appid.to_string());
                let size = if acf.state_flags & flags::ACTIVE_UPDATE != 0 { fsutil::dir_size(&dl) } else { 0 };
                if let Some(mut app) = classify(&acf, size) {
                    if app.total.is_none() {
                        app.total = self.log_totals.get(&app.appid).copied();
                    }
                    apps.push(app);
                }
            }
        }
        // Speed, order, and downloads that disappeared because they finished.
        let mut next_last = HashMap::new();
        for a in &mut apps {
            if a.phase == SteamPhase::Downloading {
                if let Some((d, t)) = self.last.get(&a.appid) {
                    let dt = now.duration_since(*t).as_secs_f64();
                    if dt > 0.2 && a.done >= *d {
                        let bps = ((a.done - d) as f64 / dt) as u64;
                        a.bytes_per_sec = Some(bps);
                        a.seconds_left = a.total.filter(|_| bps > 0).map(|t| t.saturating_sub(a.done) / bps.max(1));
                    }
                }
                next_last.insert(a.appid, (a.done, now));
            }
            if !a.update_available {
                let n = *self.seen.entry(a.appid).or_insert_with(|| {
                    self.next_seen += 1;
                    self.next_seen
                });
                a.queue_index = Some(n);
            }
        }
        for appid in self.last.keys() {
            if !next_last.contains_key(appid) && installed_now.get(appid).copied().unwrap_or(false) && !finished.contains(appid) {
                finished.push(*appid);
            }
        }
        self.seen.retain(|id, _| apps.iter().any(|a| a.appid == *id));
        self.last = next_last;
        WatchResult { apps, finished }
    }

    /// Anything moving (poll every 2 s instead of every 30 s).
    pub fn busy(&self) -> bool {
        !self.last.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Josh's real content_log.txt lines for a demo install (docs/DOWNLOADS_PLAN.md).
    const CONTENT_LOG: &str = include_str!("../tests/fixtures/content_log.txt");

    #[test]
    fn the_real_log_lines() {
        let ev: Vec<LogEvent> = CONTENT_LOG.lines().filter_map(parse_log_line).collect();
        assert_eq!(ev[0], LogEvent::State { appid: 5246870, states: vec!["Update Required".into()] });
        assert!(ev.contains(&LogEvent::Started { appid: 5246870, download: (0, 394633216), stage: (0, 1104001082) }));
        assert!(ev.contains(&LogEvent::Update { appid: 5246870, states: vec!["Running Update".into(), "Downloading".into(), "Staging".into()] }));
        assert!(ev.contains(&LogEvent::Finished { appid: 5246870, buildid: Some(26041765) }));
        assert_eq!(ev.last(), Some(&LogEvent::Update { appid: 5246870, states: vec![] }));
        // "scheduler", "preallocated" and "starting commit" lines are not events.
        assert_eq!(ev.len(), 14);
    }
}
