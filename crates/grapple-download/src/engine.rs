//! Installing and updating one Epic game (the algorithm of `docs/DOWNLOADS_PLAN.md` "Install algorithm").
//!
//! 1. Plan: which files to build (all of them for an install; new and changed ones for an update), the space they
//!    need, and for every chunk the places its bytes go.
//! 2. Preallocate every file at its final size.
//! 3. Download each chunk once (`connections` at a time, ordered by the first file that needs it), verify it,
//!    write it to every place with positional writes, drop it.
//! 4. Journal: finished chunk ids go to `.grapple-download/journal` once their data is synced. Resume skips them.
//! 5. Verify every built file (SHA-1); re-download a damaged file's chunks once.
//! 6. Finalize: executable bits, links, the manifest file and the `installed.json` entry; for an update, move
//!    staged files over the old ones and remove deleted files.
//!
//! All state of a running job lives in `<install_path>/.grapple-download/` (job.json, the manifest, the journal, the
//! update's staging folder), so a download resumes after a crash or reboot with no network call but the chunks.

use crate::chunk;
use crate::error::{Error, Result};
use crate::fsutil::{self, FreeSpaceFn};
use crate::legendary::{self, EpicMeta};
use crate::manifest::{self, FileEntry, Guid, Manifest};
use crate::ratelimit::RateLimit;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

pub const STATE_DIR: &str = ".grapple-download";
pub const GB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Install,
    Update,
}

/// Pause and cancel for a running job. Workers finish the chunk they have, then stop.
#[derive(Default)]
pub struct Control {
    state: AtomicU8,
    changed: Notify,
}

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

impl Control {
    pub fn new() -> Arc<Control> {
        Arc::new(Control::default())
    }
    pub fn pause(&self) {
        let _ = self.state.compare_exchange(RUN, PAUSE, Ordering::SeqCst, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    pub fn cancel(&self) {
        self.state.store(CANCEL, Ordering::SeqCst);
        self.changed.notify_waiters();
    }
    fn get(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }
    fn running(&self) -> bool {
        self.get() == RUN
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Starting = 0,
    Downloading = 1,
    Verifying = 2,
    Installing = 3,
}

/// Live numbers of a running job (read by the queue's ticker).
#[derive(Default)]
pub struct Progress {
    /// Download bytes of finished chunks (what "x of y GB downloaded" shows).
    pub done: AtomicU64,
    pub total: AtomicU64,
    /// Every byte received, including retries (for the speed).
    pub network: AtomicU64,
    phase: AtomicU8,
}

impl Progress {
    pub fn new() -> Arc<Progress> {
        Arc::new(Progress::default())
    }
    pub fn phase(&self) -> Phase {
        match self.phase.load(Ordering::Relaxed) {
            1 => Phase::Downloading,
            2 => Phase::Verifying,
            3 => Phase::Installing,
            _ => Phase::Starting,
        }
    }
    fn set_phase(&self, p: Phase) {
        self.phase.store(p as u8, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct EngineConfig {
    pub http: reqwest::Client,
    pub connections: usize,
    /// Waits before each retry of a chunk (0.5 s, 2 s, 5 s).
    pub retry_backoff: Vec<Duration>,
    /// How often written chunks are synced and journaled.
    pub flush_every: Duration,
    pub free_space: FreeSpaceFn,
    /// Space that must stay free besides the game.
    pub reserve: u64,
    pub limit: Arc<RateLimit>,
}

impl EngineConfig {
    pub fn new(http: reqwest::Client) -> EngineConfig {
        EngineConfig {
            http,
            connections: 8,
            retry_backoff: vec![Duration::from_millis(500), Duration::from_secs(2), Duration::from_secs(5)],
            flush_every: Duration::from_secs(3),
            free_space: fsutil::real_free_space(),
            reserve: GB,
            limit: Arc::new(RateLimit::new(0)),
        }
    }
}

/// A build to install: its manifest and the CDN folders its chunks are in.
#[derive(Clone)]
pub struct Build {
    pub manifest_bytes: Vec<u8>,
    pub manifest: Manifest,
    pub base_urls: Vec<String>,
}

impl Build {
    pub fn new(manifest_bytes: Vec<u8>, base_urls: Vec<String>) -> Result<Build> {
        let manifest = manifest::parse(&manifest_bytes)?;
        Ok(Build { manifest_bytes, manifest, base_urls })
    }
}

pub struct Job {
    pub kind: Kind,
    pub meta: EpicMeta,
    /// The game's own folder (`<drive folder>/<FolderName>`).
    pub install_path: PathBuf,
    pub legendary_dir: PathBuf,
    /// Add the install to legendary's game list (`installed.json` and its manifest). Off for Epic's overlay, which
    /// is not a game.
    pub register: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done { version: String, install_size: u64 },
    Paused,
    Cancelled,
}

/// `job.json` in the state folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFile {
    pub kind: Kind,
    pub app_name: String,
    pub build_version: String,
    pub base_urls: Vec<String>,
    /// grapple made the install folder (or it was empty): cancelling a new install may delete it.
    pub created_dir: bool,
    /// Update only: the files being rebuilt (manifest names).
    #[serde(default)]
    pub changed: Vec<String>,
}

pub fn state_dir(install_path: &Path) -> PathBuf {
    install_path.join(STATE_DIR)
}

/// The job saved in an install folder, with its build (to resume without asking Epic again).
pub fn saved(install_path: &Path) -> Option<(JobFile, Build)> {
    let dir = state_dir(install_path);
    let job: JobFile = serde_json::from_slice(&std::fs::read(dir.join("job.json")).ok()?).ok()?;
    let bytes = std::fs::read(dir.join("manifest")).ok()?;
    let build = Build::new(bytes, job.base_urls.clone()).ok()?;
    (build.manifest.meta.build_version == job.build_version).then_some((job, build))
}

fn read_journal(path: &Path) -> HashSet<Guid> {
    std::fs::read_to_string(path).map(|t| t.lines().filter_map(Guid::parse).collect()).unwrap_or_default()
}

/// One place a chunk's bytes go.
#[derive(Debug, Clone, Copy)]
struct Target {
    file: usize,
    file_offset: u64,
    chunk_offset: u32,
    size: u32,
}

struct Plan {
    /// Files to build, as indexes into the manifest's file list.
    files: Vec<usize>,
    /// Where each file is written (the install folder, or staging for an update).
    paths: Vec<PathBuf>,
    /// Chunk index → targets, in download order.
    chunks: Vec<(usize, Vec<Target>)>,
}

fn build_plan(m: &Manifest, files: Vec<usize>, root: &Path) -> Result<Plan> {
    let idx = m.chunk_index();
    let mut order: Vec<usize> = Vec::new();
    let mut targets: HashMap<usize, Vec<Target>> = HashMap::new();
    let mut paths = Vec::with_capacity(files.len());
    for (slot, &fi) in files.iter().enumerate() {
        let f = &m.files[fi];
        let rel = fsutil::safe_rel(&f.name).ok_or_else(|| Error::Format(format!("unsafe file name {:?}", f.name)))?;
        paths.push(root.join(rel));
        if f.is_symlink() {
            continue;
        }
        let mut at = 0u64;
        for p in &f.parts {
            let ci = idx[&p.guid];
            let t = targets.entry(ci).or_insert_with(|| {
                order.push(ci);
                Vec::new()
            });
            t.push(Target { file: slot, file_offset: at, chunk_offset: p.offset, size: p.size });
            at += p.size as u64;
        }
    }
    let chunks = order.into_iter().map(|ci| (ci, targets.remove(&ci).unwrap_or_default())).collect();
    Ok(Plan { files, paths, chunks })
}

/// Bytes of an update that the old build already has: copied from the old file on disk instead of downloaded.
#[derive(Debug, Clone)]
struct Reuse {
    /// The file being built (a slot of [`Plan::files`]).
    file: usize,
    file_offset: u64,
    /// The old file the bytes come from (inside the install folder), and where.
    src: PathBuf,
    src_offset: u64,
    size: u64,
}

/// The installed build's manifest (the version `installed.json` names), if it was kept.
fn old_manifest(legendary_dir: &Path, app: &str) -> Option<Manifest> {
    let entry = legendary::installed_entry(legendary_dir, app).ok()??;
    let version = entry.get("version")?.as_str()?.to_string();
    let bytes = std::fs::read(legendary::manifest_path(legendary_dir, app, &version).ok()?).ok()?;
    manifest::parse(&bytes).ok()
}

/// Turn every target whose bytes the old build has into a [`Reuse`]; a chunk is then downloaded only for the
/// targets left (chunks left with none are dropped). Sources are old files that are still the old build on disk
/// (right size, no link on the way). `changed_ok`: files the update replaces may be sources too (not once moving
/// staged files into place has begun, as those may already hold the new build). Blocking.
fn split_reuse(plan: &mut Plan, m: &Manifest, old: &Manifest, install: &Path, changed: &HashSet<&str>, changed_ok: bool) -> Vec<Reuse> {
    struct At {
        file: usize,
        pos: u64,
        chunk_offset: u32,
        size: u32,
    }
    let mut by_guid: HashMap<Guid, Vec<At>> = HashMap::new();
    let mut src: Vec<Option<PathBuf>> = vec![None; old.files.len()];
    for (fi, f) in old.files.iter().enumerate() {
        if f.is_symlink() || (!changed_ok && changed.contains(f.name.as_str())) {
            continue;
        }
        let Some(rel) = fsutil::safe_rel(&f.name) else { continue };
        let path = install.join(&rel);
        if fsutil::check_no_links(install, &rel).is_err() || !std::fs::symlink_metadata(&path).is_ok_and(|md| md.is_file() && md.len() == f.size()) {
            continue;
        }
        src[fi] = Some(path);
        let mut pos = 0u64;
        for p in &f.parts {
            by_guid.entry(p.guid).or_default().push(At { file: fi, pos, chunk_offset: p.offset, size: p.size });
            pos += p.size as u64;
        }
    }
    let mut out = Vec::new();
    for (ci, targets) in plan.chunks.iter_mut() {
        let Some(have) = by_guid.get(&m.chunks[*ci].guid) else { continue };
        targets.retain(|t| {
            let (start, end) = (t.chunk_offset as u64, t.chunk_offset as u64 + t.size as u64);
            match have.iter().find(|a| a.chunk_offset as u64 <= start && end <= a.chunk_offset as u64 + a.size as u64) {
                Some(a) => {
                    let path = src[a.file].clone().expect("only usable files are indexed");
                    out.push(Reuse { file: t.file, file_offset: t.file_offset, src: path, src_offset: a.pos + (start - a.chunk_offset as u64), size: t.size as u64 });
                    false
                }
                None => true,
            }
        });
    }
    plan.chunks.retain(|(_, t)| !t.is_empty());
    out
}

/// Copy the reused bytes into the files being built (runs that are contiguous on both sides are merged).
/// `Ok(false)`: paused or cancelled; running it again redoes it all (it only rewrites the same bytes). Blocking.
fn copy_reused(mut reuse: Vec<Reuse>, paths: &[PathBuf], control: &Control) -> Result<bool> {
    reuse.sort_by(|a, b| (&a.src, a.src_offset).cmp(&(&b.src, b.src_offset)));
    let mut runs: Vec<Reuse> = Vec::with_capacity(reuse.len());
    for r in reuse {
        if let Some(last) = runs.last_mut()
            && last.src == r.src
            && last.file == r.file
            && last.src_offset + last.size == r.src_offset
            && last.file_offset + last.size == r.file_offset
        {
            last.size += r.size;
            continue;
        }
        runs.push(r);
    }
    const OPEN_MAX: usize = 64;
    let mut buf = vec![0u8; 4 << 20];
    let mut src: Option<(PathBuf, File)> = None;
    let mut dest: HashMap<usize, File> = HashMap::new();
    let close = |dest: &mut HashMap<usize, File>| -> Result<()> {
        for (slot, f) in dest.drain() {
            f.sync_data().map_err(io_at(&paths[slot]))?;
        }
        Ok(())
    };
    for r in &runs {
        if !control.running() {
            return Ok(false);
        }
        if src.as_ref().is_none_or(|(p, _)| *p != r.src) {
            src = Some((r.src.clone(), File::open(&r.src).map_err(io_at(&r.src))?));
        }
        let s = &src.as_ref().expect("just opened").1;
        if !dest.contains_key(&r.file) {
            if dest.len() >= OPEN_MAX {
                close(&mut dest)?;
            }
            let p = &paths[r.file];
            dest.insert(r.file, fsutil::open_game_file(p, false).map_err(io_at(p))?);
        }
        let d = &dest[&r.file];
        let mut at = 0u64;
        while at < r.size {
            let n = (r.size - at).min(buf.len() as u64) as usize;
            fsutil::read_at(s, &mut buf[..n], r.src_offset + at).map_err(io_at(&r.src))?;
            fsutil::write_at(d, &buf[..n], r.file_offset + at).map_err(io_at(&paths[r.file]))?;
            at += n as u64;
        }
    }
    close(&mut dest)?;
    Ok(true)
}

fn io_at(p: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |e| Error::file(p, e)
}

/// Run a job to its end, a pause or a cancel. `build` is the build to install when no saved job exists (a saved
/// job always wins, so a resumed download never mixes two builds).
pub async fn run(cfg: &EngineConfig, job: &Job, build: Option<Build>, control: Arc<Control>, progress: Arc<Progress>) -> Result<Outcome> {
    progress.set_phase(Phase::Starting);
    let install = job.install_path.clone();
    let sdir = state_dir(&install);
    let (jobfile, build) = match (saved(&install), build) {
        (Some((jf, b)), _) if jf.kind == job.kind && jf.app_name == job.meta.app_name => (jf, b),
        (_, Some(b)) => {
            let jf = start_job(cfg, job, &b).await?;
            (jf, b)
        }
        (_, None) => return Err(Error::Invalid("nothing to resume".into())),
    };
    let m = &build.manifest;

    // Which files, written where.
    let (files, root): (Vec<usize>, PathBuf) = match job.kind {
        Kind::Install => ((0..m.files.len()).collect(), install.clone()),
        Kind::Update => {
            let names: HashSet<&str> = jobfile.changed.iter().map(String::as_str).collect();
            ((0..m.files.len()).filter(|&i| names.contains(m.files[i].name.as_str())).collect(), sdir.join("staging"))
        }
    };
    let mut plan = build_plan(m, files, &root)?;
    let journal_path = sdir.join("journal");
    let done: HashSet<Guid> = read_journal(&journal_path);
    // Staged files of an update that were already moved into place (an interrupted finalize).
    let moved: Vec<bool> = {
        let files: Vec<FileEntry> = plan.files.iter().map(|&i| m.files[i].clone()).collect();
        let (install, root) = (install.clone(), root.clone());
        if job.kind == Kind::Update {
            tokio::task::spawn_blocking(move || files.iter().map(|f| staged_and_moved(&install, &root, f)).collect())
                .await
                .map_err(|e| Error::Invalid(e.to_string()))?
        } else {
            vec![false; files.len()]
        }
    };

    // An update downloads only what the old build does not have: the rest is copied from its files on disk.
    let reuse: Vec<Reuse> = if job.kind == Kind::Update {
        let (ld, app, inst) = (job.legendary_dir.clone(), job.meta.app_name.clone(), install.clone());
        let (changed, changed_ok) = (jobfile.changed.clone(), !moved.iter().any(|&x| x));
        let (pm, mut p) = (m.clone(), std::mem::replace(&mut plan, Plan { files: Vec::new(), paths: Vec::new(), chunks: Vec::new() }));
        let (p, r) = tokio::task::spawn_blocking(move || {
            let r = match old_manifest(&ld, &app) {
                Some(old) => split_reuse(&mut p, &pm, &old, &inst, &changed.iter().map(String::as_str).collect(), changed_ok),
                None => Vec::new(),
            };
            (p, r)
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))?;
        plan = p;
        r.into_iter().filter(|r| !moved[r.file]).collect()
    } else {
        Vec::new()
    };

    // Preallocate (idempotent on resume). Staged files of an update that were already moved into place are skipped.
    {
        let plan_paths = plan.paths.clone();
        let files: Vec<FileEntry> = plan.files.iter().map(|&i| m.files[i].clone()).collect();
        let (root, moved) = (root.clone(), moved.clone());
        tokio::task::spawn_blocking(move || preallocate(&root, &plan_paths, &files, &moved)).await.map_err(|e| Error::Invalid(e.to_string()))??;
    }

    let total: u64 = plan.chunks.iter().map(|(ci, _)| m.chunks[*ci].download_size()).sum();
    progress.total.store(total, Ordering::Relaxed);
    progress.done.store(plan.chunks.iter().filter(|(ci, _)| done.contains(&m.chunks[*ci].guid)).map(|(ci, _)| m.chunks[*ci].download_size()).sum(), Ordering::Relaxed);

    let copied_mark = sdir.join("copied");
    if !reuse.is_empty() && !copied_mark.exists() {
        progress.set_phase(Phase::Starting);
        let (paths, c) = (plan.paths.clone(), control.clone());
        let finished = tokio::task::spawn_blocking(move || copy_reused(reuse, &paths, &c)).await.map_err(|e| Error::Invalid(e.to_string()))??;
        if !finished {
            return stop(&control, &install, job.kind);
        }
        std::fs::write(&copied_mark, b"").map_err(io_at(&copied_mark))?;
    }

    // Download what is missing, then verify; a damaged file is fetched again once, whole, from the CDN.
    let mut retry: Option<Plan> = None;
    loop {
        progress.set_phase(Phase::Downloading);
        let (p, todo): (&Plan, Vec<(usize, Vec<Target>)>) = match &retry {
            None => (&plan, plan.chunks.iter().filter(|(ci, _)| !done.contains(&m.chunks[*ci].guid)).cloned().collect()),
            Some(r) => (r, r.chunks.clone()),
        };
        if let Some(o) = download(cfg, &build, p, todo, &journal_path, &control, &progress).await? {
            if o == Outcome::Cancelled {
                discard_blocking(&install, job.kind);
            }
            return Ok(o);
        }
        progress.set_phase(Phase::Verifying);
        let bad = verify(&install, &root, m, &plan, job.kind).await?;
        if !control.running() {
            return stop(&control, &install, job.kind);
        }
        if bad.is_empty() {
            break;
        }
        if retry.is_some() {
            return Err(Error::Damaged);
        }
        tracing::info!("download: {} damaged files in {}, fetching them again", bad.len(), job.meta.app_name);
        let again = build_plan(m, bad.iter().map(|&slot| plan.files[slot]).collect(), &root)?;
        progress.total.fetch_add(again.chunks.iter().map(|(ci, _)| m.chunks[*ci].download_size()).sum(), Ordering::Relaxed);
        retry = Some(again);
    }

    progress.set_phase(Phase::Installing);
    let install_size = m.install_size();
    let version = m.meta.build_version.clone();
    {
        let (install, root, legendary_dir, meta, kind, register) =
            (install.clone(), root.clone(), job.legendary_dir.clone(), job.meta.clone(), job.kind, job.register);
        let b = build.clone();
        let files = plan.files.clone();
        tokio::task::spawn_blocking(move || finalize(kind, &install, &root, &legendary_dir, &meta, &b, &files, register))
            .await
            .map_err(|e| Error::Invalid(e.to_string()))??;
    }
    Ok(Outcome::Done { version, install_size })
}

fn stop(control: &Control, install: &Path, kind: Kind) -> Result<Outcome> {
    if control.get() == CANCEL {
        discard_blocking(install, kind);
        Ok(Outcome::Cancelled)
    } else {
        Ok(Outcome::Paused)
    }
}

/// First start of a job: check the folder and the space, save job.json and the manifest.
async fn start_job(cfg: &EngineConfig, job: &Job, b: &Build) -> Result<JobFile> {
    let install = job.install_path.clone();
    let m = &b.manifest;
    let (changed, need, created_dir) = match job.kind {
        Kind::Install => {
            let empty = match std::fs::read_dir(&install) {
                Ok(mut rd) => rd.next().is_none(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
                Err(e) => return Err(Error::file(&install, e)),
            };
            if !empty {
                return Err(Error::Invalid(format!("{} is not empty", install.display())));
            }
            (Vec::new(), m.install_size(), true)
        }
        Kind::Update => {
            let (install2, ld, app, new) = (install.clone(), job.legendary_dir.clone(), job.meta.app_name.clone(), b.manifest.clone());
            let changed = tokio::task::spawn_blocking(move || changed_files(&install2, &ld, &app, &new)).await.map_err(|e| Error::Invalid(e.to_string()))??;
            let need: u64 = m.files.iter().filter(|f| changed.contains(&f.name)).map(FileEntry::size).sum();
            (changed, need, false)
        }
    };
    let space = (cfg.free_space)(&install).map_err(io_at(&install))?;
    if space.free < need + cfg.reserve {
        let drive = fsutil::mount_point(&install).unwrap_or_else(|| install.clone());
        return Err(Error::NoSpace(drive.display().to_string()));
    }
    let jf = JobFile { kind: job.kind, app_name: job.meta.app_name.clone(), build_version: m.meta.build_version.clone(), base_urls: b.base_urls.clone(), created_dir, changed };
    let sdir = state_dir(&install);
    std::fs::create_dir_all(&sdir).map_err(io_at(&sdir))?;
    std::fs::write(sdir.join("manifest"), &b.manifest_bytes).map_err(io_at(&sdir))?;
    let _ = std::fs::remove_file(sdir.join("journal"));
    std::fs::write(sdir.join("job.json"), serde_json::to_vec_pretty(&jf).unwrap_or_default()).map_err(io_at(&sdir))?;
    Ok(jf)
}

/// Update: the files of the new build that are new or differ from the installed ones. With the old build's
/// manifest this is a name + hash comparison; without it, files are hashed.
fn changed_files(install: &Path, legendary_dir: &Path, app: &str, new: &Manifest) -> Result<Vec<String>> {
    let old = old_manifest(legendary_dir, app);
    let old_by: HashMap<&str, &FileEntry> = old.as_ref().map(|o| o.files.iter().map(|f| (f.name.as_str(), f)).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for f in &new.files {
        let Some(rel) = fsutil::safe_rel(&f.name) else { return Err(Error::Format(format!("unsafe file name {:?}", f.name))) };
        let path = install.join(&rel);
        let same = if f.is_symlink() {
            // Windows skips links (see `make_link`): there is nothing to compare.
            !cfg!(unix) || std::fs::read_link(&path).is_ok_and(|t| t == Path::new(&f.symlink_target))
        } else {
            let meta = std::fs::symlink_metadata(&path);
            let size_ok = meta.as_ref().is_ok_and(|m| m.is_file() && m.len() == f.size());
            match (old.is_some(), old_by.get(f.name.as_str())) {
                (true, Some(o)) => size_ok && o.sha1 == f.sha1,
                (true, None) => false,
                (false, _) => size_ok && hash_file(&path).is_ok_and(|h| h == f.sha1),
            }
        };
        if !same {
            out.push(f.name.clone());
        }
    }
    Ok(out)
}

/// An update's staged file that is already in place (a finalize that was interrupted).
fn staged_and_moved(install: &Path, staging: &Path, f: &FileEntry) -> bool {
    let Some(rel) = fsutil::safe_rel(&f.name) else { return false };
    !staging.join(&rel).exists() && std::fs::metadata(install.join(&rel)).is_ok_and(|m| m.len() == f.size()) && hash_file(&install.join(&rel)).is_ok_and(|h| h == f.sha1)
}

fn preallocate(root: &Path, paths: &[PathBuf], files: &[FileEntry], skip: &[bool]) -> Result<()> {
    std::fs::create_dir_all(root).map_err(io_at(root))?;
    for ((p, f), skip) in paths.iter().zip(files).zip(skip) {
        if f.is_symlink() || *skip {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(p);
        fsutil::check_no_links(root, rel).map_err(io_at(p))?;
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(io_at(d))?;
        }
        let file = fsutil::open_game_file(p, true).map_err(io_at(p))?;
        if file.metadata().map(|m| m.len()).unwrap_or(0) != f.size() {
            file.set_len(f.size()).map_err(io_at(p))?;
        }
    }
    Ok(())
}

/// What the workers hand the writer: a chunk's index and its uncompressed window.
type Written = (usize, Vec<u8>);

/// Download `todo`; returns `Some(Paused|Cancelled)` when stopped, `None` when every chunk is written.
async fn download(
    cfg: &EngineConfig,
    build: &Build,
    plan: &Plan,
    todo: Vec<(usize, Vec<Target>)>,
    journal_path: &Path,
    control: &Arc<Control>,
    progress: &Arc<Progress>,
) -> Result<Option<Outcome>> {
    if todo.is_empty() {
        return Ok(None);
    }
    let m = &build.manifest;
    let (tx, rx) = tokio::sync::mpsc::channel::<Written>(cfg.connections.max(1));
    let targets: HashMap<usize, Vec<Target>> = todo.iter().cloned().collect();
    let writer = {
        let paths = plan.paths.clone();
        let guids: Vec<Guid> = m.chunks.iter().map(|c| c.guid).collect();
        let journal = journal_path.to_path_buf();
        let every = cfg.flush_every;
        tokio::task::spawn_blocking(move || writer_loop(rx, targets, paths, guids, journal, every))
    };

    let mut queue = todo.into_iter().map(|(ci, _)| ci);
    let mut inflight = FuturesUnordered::new();
    let mut failure: Option<Error> = None;
    loop {
        // Cancel: drop what is in flight at once.
        if control.get() == CANCEL {
            break;
        }
        while failure.is_none() && control.running() && inflight.len() < cfg.connections.max(1) {
            let Some(ci) = queue.next() else { break };
            inflight.push(fetch_chunk(cfg, build, ci, progress.clone()));
        }
        if inflight.is_empty() {
            break;
        }
        let next = tokio::select! {
            r = inflight.next() => r,
            _ = control.changed.notified(), if control.get() != CANCEL => continue,
        };
        if control.get() == CANCEL {
            break;
        }
        match next {
            Some(Ok((ci, data))) => {
                if tx.send((ci, data)).await.is_err() {
                    failure.get_or_insert(Error::Invalid("the writer stopped".into()));
                    continue;
                }
                progress.done.fetch_add(m.chunks[ci].download_size(), Ordering::Relaxed);
            }
            Some(Err(e)) => {
                failure.get_or_insert(e);
            }
            None => break,
        }
    }
    drop(inflight);
    drop(tx);
    writer.await.map_err(|e| Error::Invalid(e.to_string()))??;
    if let Some(e) = failure {
        return Err(e);
    }
    match control.get() {
        CANCEL => Ok(Some(Outcome::Cancelled)),
        PAUSE => Ok(Some(Outcome::Paused)),
        _ => Ok(None),
    }
}

/// One chunk: try the base URLs in turn, up to `retry_backoff.len()` retries, and check what arrives.
async fn fetch_chunk(cfg: &EngineConfig, build: &Build, ci: usize, progress: Arc<Progress>) -> Result<Written> {
    let info = &build.manifest.chunks[ci];
    let path = info.path(build.manifest.chunk_dir());
    let bases = &build.base_urls;
    if bases.is_empty() {
        return Err(Error::Network("no download servers".into()));
    }
    let cap = info.download_size().max(1) * 2 + (1 << 20);
    let mut last = Error::Network("no attempt".into());
    for attempt in 0..=cfg.retry_backoff.len() {
        if attempt > 0 {
            tokio::time::sleep(cfg.retry_backoff[attempt - 1]).await;
        }
        let url = format!("{}/{path}", bases[attempt % bases.len()]);
        match get_limited(cfg, &url, cap, &progress).await.and_then(|raw| chunk::decode(&raw, info)) {
            Ok(data) => return Ok((ci, data)),
            Err(e) => {
                tracing::debug!("chunk {} attempt {}: {e}", info.guid, attempt + 1);
                last = e;
            }
        }
    }
    Err(last)
}

async fn get_limited(cfg: &EngineConfig, url: &str, cap: u64, progress: &Progress) -> Result<Vec<u8>> {
    // No auth header for chunks.
    let mut resp = cfg.http.get(url).timeout(Duration::from_secs(120)).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Network(format!("chunk: {}", resp.status())));
    }
    let mut out = Vec::with_capacity(resp.content_length().unwrap_or(0).min(cap) as usize);
    while let Some(piece) = resp.chunk().await? {
        cfg.limit.take(piece.len()).await;
        progress.network.fetch_add(piece.len() as u64, Ordering::Relaxed);
        out.extend_from_slice(&piece);
        if out.len() as u64 > cap {
            return Err(Error::Format("chunk is larger than it should be".into()));
        }
    }
    Ok(out)
}

/// The one thread that writes: positional writes into every target, then (every `every`) sync the touched files
/// and append their chunk ids to the journal. The journal never names a chunk whose data is not on disk.
fn writer_loop(
    mut rx: tokio::sync::mpsc::Receiver<Written>,
    targets: HashMap<usize, Vec<Target>>,
    paths: Vec<PathBuf>,
    guids: Vec<Guid>,
    journal: PathBuf,
    every: Duration,
) -> Result<()> {
    const OPEN_MAX: usize = 64;
    let mut open: HashMap<usize, File> = HashMap::new();
    let mut dirty: HashSet<usize> = HashSet::new();
    let mut pending: Vec<Guid> = Vec::new();
    let mut last = Instant::now();
    let mut jf = OpenOptions::new().create(true).append(true).open(&journal).map_err(io_at(&journal))?;

    let flush = |open: &mut HashMap<usize, File>, dirty: &mut HashSet<usize>, pending: &mut Vec<Guid>, jf: &mut File| -> Result<()> {
        for slot in dirty.drain() {
            let p = &paths[slot];
            match open.get(&slot) {
                Some(f) => f.sync_data().map_err(io_at(p))?,
                None => fsutil::open_game_file(p, false).and_then(|f| f.sync_data()).map_err(io_at(p))?,
            }
        }
        if !pending.is_empty() {
            let mut text = String::with_capacity(pending.len() * 33);
            for g in pending.drain(..) {
                text.push_str(&g.to_string());
                text.push('\n');
            }
            jf.write_all(text.as_bytes()).and_then(|_| jf.sync_data()).map_err(io_at(&journal))?;
        }
        Ok(())
    };

    while let Some((ci, data)) = rx.blocking_recv() {
        for t in targets.get(&ci).map(Vec::as_slice).unwrap_or_default() {
            if !open.contains_key(&t.file) {
                if open.len() >= OPEN_MAX {
                    // Closing is fine: the dirty set syncs by path.
                    open.clear();
                }
                let p = &paths[t.file];
                open.insert(t.file, fsutil::open_game_file(p, false).map_err(io_at(p))?);
            }
            let f = &open[&t.file];
            let end = t.chunk_offset as usize + t.size as usize;
            fsutil::write_at(f, &data[t.chunk_offset as usize..end], t.file_offset).map_err(io_at(&paths[t.file]))?;
            dirty.insert(t.file);
        }
        pending.push(guids[ci]);
        if last.elapsed() >= every {
            flush(&mut open, &mut dirty, &mut pending, &mut jf)?;
            last = Instant::now();
        }
    }
    flush(&mut open, &mut dirty, &mut pending, &mut jf)
}

pub fn hash_file(p: &Path) -> std::io::Result<[u8; 20]> {
    use sha1::{Digest, Sha1};
    let mut f = File::open(p)?;
    let mut h = Sha1::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().into())
}

/// Hash every built file; returns the plan slots that do not match. Uses a few threads.
async fn verify(install: &Path, root: &Path, m: &Manifest, plan: &Plan, kind: Kind) -> Result<Vec<usize>> {
    let jobs: Vec<(usize, PathBuf, PathBuf, [u8; 20])> = plan
        .files
        .iter()
        .enumerate()
        .filter(|(_, fi)| !m.files[**fi].is_symlink())
        .map(|(slot, fi)| {
            let staged = plan.paths[slot].clone();
            let rel = staged.strip_prefix(root).map(Path::to_path_buf).unwrap_or_default();
            (slot, staged, install.join(rel), m.files[*fi].sha1)
        })
        .collect();
    tokio::task::spawn_blocking(move || {
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 4);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let bad = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some((slot, staged, final_path, want)) = jobs.get(i) else { break };
                        // An update's staged file that was already moved: check it in place.
                        let p = if kind == Kind::Update && !staged.exists() { final_path } else { staged };
                        if hash_file(p).map(|h| h != *want).unwrap_or(true) {
                            bad.lock().unwrap().push(*slot);
                        }
                    }
                });
            }
        });
        bad.into_inner().unwrap()
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))
}

#[allow(clippy::too_many_arguments)]
fn finalize(kind: Kind, install: &Path, root: &Path, legendary_dir: &Path, meta: &EpicMeta, b: &Build, files: &[usize], register: bool) -> Result<()> {
    let m = &b.manifest;
    for &fi in files {
        let f = &m.files[fi];
        let rel = fsutil::safe_rel(&f.name).ok_or_else(|| Error::Format("unsafe file name".into()))?;
        let dest = install.join(&rel);
        if kind == Kind::Update && !f.is_symlink() {
            let staged = root.join(&rel);
            if staged.exists() {
                fsutil::check_no_links(install, &rel).map_err(io_at(&dest))?;
                if let Some(d) = dest.parent() {
                    std::fs::create_dir_all(d).map_err(io_at(d))?;
                }
                std::fs::rename(&staged, &dest).map_err(io_at(&dest))?;
            }
        }
        if f.is_symlink() {
            make_link(install, &rel, &f.symlink_target)?;
        } else if f.executable() {
            fsutil::set_executable(&dest).map_err(io_at(&dest))?;
        }
    }
    if kind == Kind::Update {
        // Files the old build had and the new one does not.
        let entry = legendary::installed_entry(legendary_dir, &meta.app_name)?;
        let old_version = entry.as_ref().and_then(|e| e.get("version")).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
        if let Some(old) = legendary::manifest_path(legendary_dir, &meta.app_name, &old_version).ok().and_then(|p| std::fs::read(p).ok()).and_then(|bytes| manifest::parse(&bytes).ok()) {
            let keep: HashSet<&str> = m.files.iter().map(|f| f.name.as_str()).collect();
            for f in old.files.iter().filter(|f| !keep.contains(f.name.as_str())) {
                if let Some(rel) = fsutil::safe_rel(&f.name)
                    && fsutil::check_no_links(install, &rel).is_ok()
                {
                    let _ = std::fs::remove_file(install.join(rel));
                }
            }
        }
    }
    if register {
        legendary::write_manifest_file(legendary_dir, &meta.app_name, &m.meta.build_version, &b.manifest_bytes)?;
        legendary::upsert_installed(legendary_dir, &meta.app_name, legendary::entry_fields(m, meta, install, &b.base_urls))?;
    }
    let sdir = state_dir(install);
    std::fs::remove_dir_all(&sdir).map_err(io_at(&sdir))?;
    Ok(())
}

#[cfg(unix)]
fn make_link(install: &Path, rel: &Path, target: &str) -> Result<()> {
    let dest = install.join(rel);
    fsutil::check_no_links(install, rel).map_err(io_at(&dest))?;
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(io_at(d))?;
    }
    if std::fs::symlink_metadata(&dest).is_ok() {
        let _ = std::fs::remove_file(&dest);
    }
    std::os::unix::fs::symlink(target.replace('\\', "/"), &dest).map_err(io_at(&dest))
}

#[cfg(not(unix))]
fn make_link(_install: &Path, rel: &Path, _target: &str) -> Result<()> {
    tracing::warn!("download: skipping the link {} (links need extra rights on Windows)", rel.display());
    Ok(())
}

/// Throw away a job that is not running: a new install's folder (only when grapple made it), or an update's
/// state folder (the game stays playable). Blocking.
pub fn discard_blocking(install: &Path, kind: Kind) {
    let sdir = state_dir(install);
    let created = std::fs::read(sdir.join("job.json")).ok().and_then(|b| serde_json::from_slice::<JobFile>(&b).ok()).is_some_and(|j| j.created_dir && j.kind == Kind::Install);
    let home = grapple_launch::paths::home();
    if kind == Kind::Install && created && fsutil::deletable_game_dir(install, &home) {
        if let Err(e) = std::fs::remove_dir_all(install) {
            tracing::warn!("download: could not remove {}: {e}", install.display());
        }
    } else if sdir.exists() {
        let _ = std::fs::remove_dir_all(&sdir);
    }
}
