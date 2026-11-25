//! End-to-end installs and updates against the fake Epic in `common/` (local axum server, temp folders).

mod common;

use common::*;
use grapple_download::engine::{self, Build, Control, EngineConfig, Job, Kind, Outcome, Progress};
use grapple_download::epic::{EpicClient, TokenSource};
use grapple_download::fsutil::Space;
use grapple_download::legendary::{self, EpicMeta};
use grapple_download::ratelimit::RateLimit;
use grapple_download::manifest::{ChunkInfo, ChunkPart, Guid, sha1};
use grapple_download::testkit::{Built, GameSpec, TestFile, build, chunk_file, noise, sample_files, write_manifest};
use grapple_download::Error;
use grapple_launch::epic_auth::{EpicApi, EpicAuth};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime};

const W: u32 = 16 * 1024;

struct World {
    _tmp: tempfile::TempDir,
    epic: FakeEpic,
    legendary: PathBuf,
    games: PathBuf,
    auth: EpicAuth,
}

async fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let legendary = tmp.path().join("legendary");
    std::fs::create_dir_all(&legendary).unwrap();
    let auth = EpicAuth::new(EpicApi { oauth_base: "http://127.0.0.1:9".into(), ..EpicApi::default() }, fake_login(tmp.path())).unwrap();
    let games = tmp.path().join("Games");
    World { epic: FakeEpic::start().await, legendary, games, auth, _tmp: tmp }
}

fn spec(version: &str, seed: u32, files: Vec<TestFile>) -> GameSpec {
    GameSpec { app_name: "Sugar".into(), build_version: version.into(), window: W, seed, files, ..GameSpec::default() }
}

fn cfg() -> EngineConfig {
    let mut c = EngineConfig::new(reqwest::Client::new());
    c.retry_backoff = vec![Duration::from_millis(5), Duration::from_millis(10), Duration::from_millis(20)];
    c.flush_every = Duration::from_millis(20);
    c.connections = 4;
    c.free_space = Arc::new(|_| Ok(Space { free: 500 * 1024 * 1024 * 1024, total: 1 << 40 }));
    c.reserve = 1 << 30;
    c
}

async fn fetch_build(w: &World, app: &str) -> Build {
    let client = EpicClient::new(w.epic.base()).unwrap();
    let token = w.auth.token().await.unwrap();
    let loc = client.manifest_location(&token, "ns1", "cat1", app).await.unwrap();
    let bytes = client.manifest(&loc).await.unwrap();
    Build::new(bytes, loc.base_urls).unwrap()
}

fn job(w: &World, kind: Kind) -> Job {
    write_metadata(&w.legendary, "Sugar", "Rocket League®", Some("rocketleague"));
    let meta = legendary::read_metadata(&w.legendary, "Sugar").unwrap();
    Job { kind, install_path: w.games.join(legendary::folder_name(&meta)), meta, legendary_dir: w.legendary.clone(), register: true }
}

fn assert_installed(dir: &Path, files: &[TestFile]) {
    for f in files {
        let p = dir.join(&f.name);
        if let Some(t) = &f.symlink {
            // Windows skips links (they need extra rights there).
            if cfg!(unix) {
                assert_eq!(std::fs::read_link(&p).unwrap(), Path::new(t), "{}", f.name);
            }
        } else {
            assert_eq!(std::fs::read(&p).unwrap(), f.data, "{} differs", f.name);
        }
    }
}

fn seed_other_game(legendary: &Path) -> Value {
    let other = json!({"Other": {"app_name": "Other", "install_path": "/x/Other", "version": "7", "unknown_field": {"keep": [1, 2]}, "title": "Other"}});
    std::fs::write(legendary.join("installed.json"), serde_json::to_vec_pretty(&other).unwrap()).unwrap();
    other["Other"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn install_end_to_end() {
    let w = world().await;
    let mut files = sample_files(W as usize, 3);
    files.push(TestFile { name: "Binaries/link".into(), symlink: Some("Win64/Game.exe".into()), ..TestFile::default() });
    let built = build(&spec("1.0-CL-1", 1, files.clone()));
    let paths: Vec<String> = built.chunks.keys().cloned().collect();
    {
        let mut s = w.epic.state.lock().unwrap();
        // CDN 1 fails some chunks; one chunk is corrupt once.
        s.cdn1_fails.insert(paths[0].clone());
        s.cdn1_fails.insert(paths[2].clone());
        s.corrupt_once.insert(paths[1].clone());
    }
    let manifest_bytes = built.manifest_bytes.clone();
    let install_size = built.manifest.install_size();
    w.epic.add("Sugar", built);
    let other = seed_other_game(&w.legendary);

    let b = fetch_build(&w, "Sugar").await;
    assert_eq!(b.base_urls.len(), 2, "two CDNs");
    let j = job(&w, Kind::Install);
    let p = Progress::new();
    let out = engine::run(&cfg(), &j, Some(b), Control::new(), p.clone()).await.unwrap();
    assert_eq!(out, Outcome::Done { version: "1.0-CL-1".into(), install_size });
    assert_eq!(p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));

    assert_installed(&j.install_path, &files);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |n: &str| std::fs::metadata(j.install_path.join(n)).unwrap().permissions().mode();
        assert_ne!(mode("Binaries/Win64/Game.exe") & 0o111, 0, "executable flag");
        assert_eq!(mode("readme.txt") & 0o111, 0);
    }
    assert!(!j.install_path.join(".grapple-download").exists(), "state folder removed");

    // installed.json: the other game untouched, ours exactly as specified.
    let all: Value = serde_json::from_slice(&std::fs::read(w.legendary.join("installed.json")).unwrap()).unwrap();
    assert_eq!(all["Other"], other);
    let host = format!("http://{}", w.epic.addr);
    assert_eq!(
        all["Sugar"],
        json!({
            "app_name": "Sugar",
            "base_urls": [format!("{host}/c/1/Builds/Sugar/default"), format!("{host}/c/2/Builds/Sugar/default")],
            "can_run_offline": true,
            "egl_guid": "",
            "executable": "Binaries/Win64/Game.exe",
            "install_path": j.install_path.to_string_lossy(),
            "install_size": install_size,
            "install_tags": [],
            "is_dlc": false,
            "is_preloaded": false,
            "launch_parameters": "-nosplash",
            "manifest_path": null,
            "needs_verification": false,
            "platform": "Windows",
            "prereq_info": {"args": "install abc", "ids": ["EAC_Service"], "name": "", "path": "EasyAntiCheat/Setup.exe"},
            "requires_ot": false,
            "save_path": null,
            "save_timestamp": null,
            "title": "Rocket League®",
            "uninstaller": {"args": "uninstall abc", "path": "EasyAntiCheat/Setup.exe"},
            "use_signed_url": false,
            "version": "1.0-CL-1"
        })
    );
    // The manifest file, raw bytes, under legendary's name.
    assert_eq!(std::fs::read(w.legendary.join("manifests/Sugar_Windows_1.0-CL-1.manifest")).unwrap(), manifest_bytes);
    // grapple-launch reads the entry back.
    let ig = grapple_launch::legendary::InstalledGame::load(&w.legendary, "Sugar").unwrap();
    assert_eq!(ig.executable, "Binaries/Win64/Game.exe");

    let s = w.epic.state.lock().unwrap();
    assert!(!s.chunk_auth_seen, "chunks are fetched without an auth header");
    assert!(s.service_auth.iter().all(|a| a.eq_ignore_ascii_case(&format!("bearer {TOKEN}"))));
    // The failing chunks were fetched from CDN 2 after CDN 1 said 500.
    assert!(s.chunk_requests.contains(&(1, paths[0].clone())) && s.chunk_requests.contains(&(2, paths[0].clone())));
    assert_eq!(s.chunk_requests.iter().filter(|r| r.1 == paths[1]).count(), 2, "the corrupt chunk was fetched again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manifest_url_without_query_and_signed_urls() {
    let w = world().await;
    w.epic.add("Sugar", build(&spec("1", 1, sample_files(W as usize, 1))));
    w.epic.state.lock().unwrap().with_query = false;
    let b = fetch_build(&w, "Sugar").await;
    assert_eq!(b.manifest.meta.build_version, "1");
    w.epic.state.lock().unwrap().signed = true;
    let client = EpicClient::new(w.epic.base()).unwrap();
    let token = w.auth.token().await.unwrap();
    let e = client.manifest_location(&token, "ns1", "cat1", "Sugar").await.unwrap_err();
    assert!(matches!(e, Error::SignedUrl));
    assert_eq!(e.short(), "This game needs Heroic to install for now");
    // A wrong token is a signed-out error.
    let bad = grapple_launch::plan::Secret::new("nope");
    assert!(matches!(client.assets(&bad).await, Err(Error::SignedOut)));
    assert_eq!(client.assets(&token).await.unwrap()[0].app_name, "Sugar");
}

fn big_game(seed: u32, windows: usize) -> Vec<TestFile> {
    vec![TestFile::new("big.pak", noise(seed as u64, W as usize * windows)), TestFile::new("small.txt", b"hi".to_vec())]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pause_and_resume_from_the_journal() {
    let w = world().await;
    let files = big_game(5, 24);
    w.epic.add("Sugar", build(&spec("1", 1, files.clone())));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let mut c = cfg();
    c.connections = 2;
    c.limit = Arc::new(RateLimit::new(W as u64 * 8)); // ~8 chunks a second
    let control = Control::new();
    let p = Progress::new();
    let (c2, p2) = (control.clone(), p.clone());
    let pauser = tokio::spawn(async move {
        while p2.done.load(Ordering::Relaxed) < W as u64 * 6 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        c2.pause();
    });
    let out = engine::run(&c, &j, Some(b.clone()), control, p.clone()).await.unwrap();
    pauser.await.unwrap();
    assert_eq!(out, Outcome::Paused);
    let journal = std::fs::read_to_string(j.install_path.join(".grapple-download/journal")).unwrap();
    let journaled = journal.lines().count();
    assert!((6..25).contains(&journaled), "journal has {journaled}");
    let before = w.epic.requests().len();

    // Resume with no build given: the saved job and manifest are used.
    let out = engine::run(&cfg(), &j, None, Control::new(), Progress::new()).await.unwrap();
    assert!(matches!(out, Outcome::Done { .. }));
    assert_installed(&j.install_path, &files);
    let after = w.epic.requests().len() - before;
    assert_eq!(after, b.manifest.chunks.len() - journaled, "journaled chunks are not fetched again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resume_after_an_abort_mid_way() {
    let w = world().await;
    let files = big_game(6, 30);
    w.epic.add("Sugar", build(&spec("1", 1, files.clone())));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let mut c = cfg();
    c.limit = Arc::new(RateLimit::new(W as u64 * 20));
    // The process "dies": the future is dropped wherever it is.
    let _ = tokio::time::timeout(Duration::from_millis(700), engine::run(&c, &j, Some(b), Control::new(), Progress::new())).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let journaled = std::fs::read_to_string(j.install_path.join(".grapple-download/journal")).unwrap_or_default().lines().count();
    assert!(journaled > 0, "some chunks were journaled before the abort");
    let out = engine::run(&cfg(), &j, None, Control::new(), Progress::new()).await.unwrap();
    assert!(matches!(out, Outcome::Done { .. }));
    assert_installed(&j.install_path, &files);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_cleans_up() {
    let w = world().await;
    w.epic.add("Sugar", build(&spec("1", 1, big_game(7, 20))));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let mut c = cfg();
    c.limit = Arc::new(RateLimit::new(W as u64 * 10));
    let control = Control::new();
    let c2 = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(400)).await;
        c2.cancel();
    });
    let out = engine::run(&c, &j, Some(b.clone()), control, Progress::new()).await.unwrap();
    assert_eq!(out, Outcome::Cancelled);
    assert!(!j.install_path.exists(), "a cancelled new install leaves nothing");
    assert!(w.games.exists(), "the drive folder stays");

    // A cancelled install that is not running: discard removes it too.
    let control = Control::new();
    let c2 = control.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.pause();
    });
    assert_eq!(engine::run(&c, &j, Some(b), control, Progress::new()).await.unwrap(), Outcome::Paused);
    assert!(j.install_path.exists());
    engine::discard_blocking(&j.install_path, Kind::Install);
    assert!(!j.install_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_that_is_not_empty_is_refused() {
    let w = world().await;
    w.epic.add("Sugar", build(&spec("1", 1, sample_files(W as usize, 1))));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    std::fs::create_dir_all(&j.install_path).unwrap();
    std::fs::write(j.install_path.join("mine.txt"), b"keep me").unwrap();
    assert!(engine::run(&cfg(), &j, Some(b), Control::new(), Progress::new()).await.is_err());
    engine::discard_blocking(&j.install_path, Kind::Install);
    assert_eq!(std::fs::read(j.install_path.join("mine.txt")).unwrap(), b"keep me", "never deleted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn not_enough_space_is_refused() {
    let w = world().await;
    w.epic.add("Sugar", build(&spec("1", 1, sample_files(W as usize, 1))));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let mut c = cfg();
    // Room for the game but not the 1 GB that must stay free.
    c.free_space = Arc::new(|_| Ok(Space { free: 1 << 29, total: 1 << 40 }));
    let e = engine::run(&c, &j, Some(b), Control::new(), Progress::new()).await.unwrap_err();
    assert!(matches!(e, Error::NoSpace(_)), "{e}");
    assert!(e.to_string().starts_with("Not enough space on "));
    assert_eq!(e.short(), "Not enough space");
    assert!(w.epic.requests().is_empty(), "nothing downloaded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn speed_limit_holds() {
    let w = world().await;
    w.epic.add("Sugar", build(&spec("1", 1, big_game(9, 40))));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let rate = 320 * 1024u64;
    let mut c = cfg();
    c.limit = Arc::new(RateLimit::new(rate));
    let p = Progress::new();
    let t = Instant::now();
    engine::run(&c, &j, Some(b), Control::new(), p.clone()).await.unwrap();
    let secs = t.elapsed().as_secs_f64();
    let speed = p.network.load(Ordering::Relaxed) as f64 / secs;
    // A limit is a ceiling: a slow machine (Windows CI managed 60 KB/s) may stay well under it.
    assert!(speed <= rate as f64 * 1.2, "speed {speed:.0} B/s vs limit {rate} ({secs:.2}s)");
}

fn set_old_mtime(p: &Path) -> SystemTime {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    std::fs::metadata(p).unwrap().modified().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_v1_to_v2() {
    let w = world().await;
    let v1 = vec![
        TestFile::new("same/a.bin", noise(1, 40_000)),
        TestFile::new("same/b.txt", b"unchanged".to_vec()),
        TestFile::new("changes.pak", noise(2, 50_000)),
        TestFile::new("goes/away.dat", noise(3, 1000)),
    ];
    let mut v2 = vec![
        v1[0].clone(),
        v1[1].clone(),
        TestFile::new("changes.pak", noise(22, 60_000)),
        TestFile::new("new/added.bin", noise(4, 30_000)).exec(),
    ];
    w.epic.add("Sugar", build(&spec("1.0", 1, v1.clone())));
    let j = job(&w, Kind::Install);
    let b1 = fetch_build(&w, "Sugar").await;
    engine::run(&cfg(), &j, Some(b1), Control::new(), Progress::new()).await.unwrap();
    let mt_a = set_old_mtime(&j.install_path.join("same/a.bin"));
    let mt_b = set_old_mtime(&j.install_path.join("same/b.txt"));

    w.epic.add("Sugar", build(&spec("2.0", 2, v2.clone())));
    let b2 = fetch_build(&w, "Sugar").await;
    let changed_chunks: usize = b2.manifest.chunks.len();
    let before = w.epic.requests().len();

    // An update that is cancelled leaves the game playable.
    let up = Job { kind: Kind::Update, ..job(&w, Kind::Update) };
    let control = Control::new();
    control.cancel();
    let out = engine::run(&cfg(), &up, Some(b2.clone()), control, Progress::new()).await.unwrap();
    assert_eq!(out, Outcome::Cancelled);
    assert!(!up.install_path.join(".grapple-download").exists());
    assert_installed(&up.install_path, &v1);

    let p = Progress::new();
    let out = engine::run(&cfg(), &up, Some(b2), Control::new(), p.clone()).await.unwrap();
    assert!(matches!(out, Outcome::Done { ref version, .. } if version == "2.0"));
    assert_installed(&up.install_path, &v2);
    assert!(!up.install_path.join("goes/away.dat").exists(), "removed file is gone");
    assert_eq!(std::fs::metadata(up.install_path.join("same/a.bin")).unwrap().modified().unwrap(), mt_a, "unchanged file not rewritten");
    assert_eq!(std::fs::metadata(up.install_path.join("same/b.txt")).unwrap().modified().unwrap(), mt_b, "unchanged file not rewritten");
    // Only the chunks of changed and new files were fetched.
    let fetched = w.epic.requests().len() - before;
    assert!(fetched < changed_chunks, "fetched {fetched} of {changed_chunks} chunks");
    let entry = legendary::installed_entry(&w.legendary, "Sugar").unwrap().unwrap();
    assert_eq!(entry["version"], "2.0");
    assert!(w.legendary.join("manifests/Sugar_Windows_1.0.manifest").exists(), "old manifest kept");
    assert!(w.legendary.join("manifests/Sugar_Windows_2.0.manifest").exists());
    v2.clear();
}

#[tokio::test]
async fn metadata_and_folder_names() {
    let w = world().await;
    write_metadata(&w.legendary, "abc", "Fall Guys", None);
    let m: EpicMeta = legendary::read_metadata(&w.legendary, "abc").unwrap();
    assert_eq!((m.title.as_str(), m.namespace.as_str(), m.catalog_item_id.as_str(), m.can_run_offline), ("Fall Guys", "ns1", "cat1", true));
    assert_eq!(legendary::folder_name(&m), "Fall Guys");
    assert!(legendary::read_metadata(&w.legendary, "../etc").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_repairs_a_damaged_file_once() {
    let w = world().await;
    let files = big_game(11, 12);
    w.epic.add("Sugar", build(&spec("1", 1, files.clone())));
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let control = Control::new();
    let p = Progress::new();
    let (c2, p2) = (control.clone(), p.clone());
    tokio::spawn(async move {
        while p2.done.load(Ordering::Relaxed) < W as u64 * 4 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        c2.pause();
    });
    let mut c = cfg();
    c.limit = Arc::new(RateLimit::new(W as u64 * 10));
    assert_eq!(engine::run(&c, &j, Some(b), control, p).await.unwrap(), Outcome::Paused);
    // Damage bytes that the journal says are written (as a crash before the data reached the disk would).
    let big = j.install_path.join("big.pak");
    let mut data = std::fs::read(&big).unwrap();
    data[10] ^= 0xFF;
    std::fs::write(&big, &data).unwrap();
    let out = engine::run(&cfg(), &j, None, Control::new(), Progress::new()).await.unwrap();
    assert!(matches!(out, Outcome::Done { .. }));
    assert_installed(&j.install_path, &files);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_that_stays_damaged_fails_the_download() {
    let w = world().await;
    let mut built = build(&spec("1", 1, sample_files(W as usize, 2)));
    // The manifest promises a hash no data has.
    built.manifest.files[5].sha1 = [1; 20];
    built.manifest_bytes = grapple_download::testkit::write_manifest(&built.manifest, true, 2);
    w.epic.add("Sugar", built);
    let b = fetch_build(&w, "Sugar").await;
    let j = job(&w, Kind::Install);
    let e = engine::run(&cfg(), &j, Some(b), Control::new(), Progress::new()).await.unwrap_err();
    assert!(matches!(e, Error::Damaged));
    assert_eq!(e.short(), "Download damaged, try again");
}

/// Version 2 of a built game where `name` gets `extra` appended, built the way Epic builds updates: the file's old
/// bytes keep their chunks, the new bytes get one new chunk. Returns the build and that chunk's download size.
fn appended(v1: &Built, version: &str, name: &str, old: &[u8], extra: &[u8]) -> (Built, u64) {
    let mut m = v1.manifest.clone();
    m.meta.build_version = version.into();
    let g = Guid([0xBEEF, 1, 2, 3]);
    let mut window = extra.to_vec();
    window.resize(W as usize, 0);
    let file = chunk_file(g, 0x5555, &window, true);
    let info = ChunkInfo { guid: g, rolling_hash: 0x5555, sha1: sha1(&window), group: 7, window_size: W, file_size: file.len() as i64 };
    let mut chunks = v1.chunks.clone();
    chunks.insert(info.path(m.chunk_dir()), file);
    let size = info.download_size();
    m.chunks.push(info);
    let f = m.files.iter_mut().find(|f| f.name == name).unwrap();
    f.parts.push(ChunkPart { guid: g, offset: 0, size: extra.len() as u32 });
    let mut data = old.to_vec();
    data.extend_from_slice(extra);
    f.sha1 = sha1(&data);
    (Built { manifest_bytes: write_manifest(&m, true, 2), manifest: m, chunks }, size)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_copies_what_the_old_build_has() {
    let w = world().await;
    let old_pak = noise(5, 200_000);
    let v1 = vec![TestFile::new("Content/big.pak", old_pak.clone()), TestFile::new("other.bin", noise(6, 30_000))];
    let built1 = build(&spec("1.0", 1, v1.clone()));
    let extra = noise(7, 5_000);
    let (built2, new_size) = appended(&built1, "2.0", "Content/big.pak", &old_pak, &extra);
    w.epic.add("Sugar", built1);
    let j = job(&w, Kind::Install);
    let b1 = fetch_build(&w, "Sugar").await;
    engine::run(&cfg(), &j, Some(b1), Control::new(), Progress::new()).await.unwrap();

    w.epic.add("Sugar", built2);
    let b2 = fetch_build(&w, "Sugar").await;
    let before = w.epic.requests().len();
    let up = job(&w, Kind::Update);
    let p = Progress::new();
    let out = engine::run(&cfg(), &up, Some(b2), Control::new(), p.clone()).await.unwrap();
    assert!(matches!(out, Outcome::Done { ref version, .. } if version == "2.0"));
    let mut want = old_pak.clone();
    want.extend_from_slice(&extra);
    assert_eq!(std::fs::read(up.install_path.join("Content/big.pak")).unwrap(), want);
    assert_eq!(w.epic.requests().len() - before, 1, "only the new chunk is downloaded");
    assert_eq!(p.total.load(Ordering::Relaxed), new_size, "the size shown is the real download");
    assert!(!up.install_path.join(".grapple-download").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn update_downloads_again_when_the_old_file_is_damaged() {
    let w = world().await;
    let old_pak = noise(5, 200_000);
    let v1 = vec![TestFile::new("Content/big.pak", old_pak.clone())];
    let built1 = build(&spec("1.0", 1, v1));
    let extra = noise(7, 5_000);
    let (built2, _) = appended(&built1, "2.0", "Content/big.pak", &old_pak, &extra);
    w.epic.add("Sugar", built1);
    let j = job(&w, Kind::Install);
    let b1 = fetch_build(&w, "Sugar").await;
    engine::run(&cfg(), &j, Some(b1), Control::new(), Progress::new()).await.unwrap();
    // Same size, one byte flipped: the copy brings the damage along, verifying catches it.
    let pak = j.install_path.join("Content/big.pak");
    let mut bytes = std::fs::read(&pak).unwrap();
    bytes[1234] ^= 0xFF;
    std::fs::write(&pak, bytes).unwrap();

    w.epic.add("Sugar", built2);
    let b2 = fetch_build(&w, "Sugar").await;
    let up = job(&w, Kind::Update);
    let out = engine::run(&cfg(), &up, Some(b2), Control::new(), Progress::new()).await.unwrap();
    assert!(matches!(out, Outcome::Done { .. }));
    let mut want = old_pak.clone();
    want.extend_from_slice(&extra);
    assert_eq!(std::fs::read(up.install_path.join("Content/big.pak")).unwrap(), want, "fetched whole from the CDN");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eos_overlay_installs_updates_and_is_not_a_game() {
    use grapple_download::eos_overlay::{self, APP_NAME};
    use grapple_launch::eos::MARKER;
    let w = world().await;
    let files = vec![TestFile::new(MARKER, noise(1, 30_000)), TestFile::new("EOSOverlayRenderer-Win64-Shipping.exe", noise(2, 20_000))];
    let mut s1 = spec("1.0", 1, files.clone());
    s1.app_name = APP_NAME.into();
    w.epic.add(APP_NAME, build(&s1));
    let client = EpicClient::new(w.epic.base()).unwrap();
    let token = w.auth.token().await.unwrap();
    let dir = w.games.join("eos-overlay");

    assert_eq!(eos_overlay::install(&cfg(), &client, &token, &dir, &w.legendary).await.unwrap(), "1.0");
    assert_eq!(eos_overlay::installed_version(&dir).as_deref(), Some("1.0"));
    assert_installed(&dir, &files);
    assert!(legendary::installed_entry(&w.legendary, APP_NAME).unwrap().is_none(), "the overlay is not added to legendary's games");

    // Same build: nothing is downloaded again.
    let before = w.epic.requests().len();
    assert_eq!(eos_overlay::install(&cfg(), &client, &token, &dir, &w.legendary).await.unwrap(), "1.0");
    assert_eq!(w.epic.requests().len(), before);

    // A new build replaces the whole folder.
    let files2 = vec![TestFile::new(MARKER, noise(3, 31_000))];
    let mut s2 = spec("2.0", 2, files2.clone());
    s2.app_name = APP_NAME.into();
    w.epic.add(APP_NAME, build(&s2));
    assert_eq!(eos_overlay::install(&cfg(), &client, &token, &dir, &w.legendary).await.unwrap(), "2.0");
    assert_installed(&dir, &files2);
    assert!(!dir.join("EOSOverlayRenderer-Win64-Shipping.exe").exists(), "files of the old build are gone");
    assert!(!w.games.join("eos-overlay.new").exists() && !w.games.join("eos-overlay.old").exists());
}
