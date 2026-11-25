//! The profile-picture cache: `$XDG_CACHE_HOME/grapple/avatars/<user id>-<hash>.<ext>`.
//!
//! One file per user and picture. A picture is downloaded once per (user, hash) by the tether service (public
//! route, no token; the fetch itself lives in `grapple_tether::Api::avatar`: https/loopback/tailnet origin only,
//! size cap, image types only, timeout, no redirects). Storing a new hash for a user deletes that user's older
//! files. Nothing is trusted on its name: bytes are re-checked when stored and when found on disk.
//!
//! `ready` is what the `grapple://avatar/<user id>` protocol may serve: only people in the current snapshot
//! (the service calls [`AvatarCache::retain`] with them) and only a file that is really on disk.

use crate::backend::AvatarFile;
use grapple_tether::{AVATAR_MAX_BYTES, AvatarImage, ImageKind, valid_avatar_hash, valid_user_id};
use std::collections::HashMap;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

const KINDS: [ImageKind; 4] = [ImageKind::Png, ImageKind::Jpeg, ImageKind::Webp, ImageKind::Gif];

pub struct AvatarCache {
    dir: PathBuf,
    ready: RwLock<HashMap<String, AvatarFile>>,
}

fn file_name(id: &str, hash: &str, kind: ImageKind) -> String {
    format!("{id}-{hash}.{}", kind.ext())
}

/// `(user id, hash, kind)` from a cache file name, or `None` for anything else (temp files, strangers).
fn parse_name(name: &str) -> Option<(&str, &str, ImageKind)> {
    let (id, rest) = name.split_once('-')?;
    let (hash, ext) = rest.rsplit_once('.')?;
    (valid_user_id(id) && valid_avatar_hash(hash)).then_some(())?;
    Some((id, hash, ImageKind::from_ext(ext)?))
}

/// The URL the UI puts in `<img src>`. `?v=<hash>` makes a changed picture a new URL.
pub fn avatar_url(id: &str, hash: &str) -> String {
    if cfg!(windows) { format!("http://grapple.localhost/avatar/{id}?v={hash}") } else { format!("grapple://avatar/{id}?v={hash}") }
}

impl AvatarCache {
    pub fn new(dir: PathBuf) -> AvatarCache {
        AvatarCache { dir, ready: RwLock::new(HashMap::new()) }
    }

    /// The file for exactly this (user, hash), if it is on disk and still looks like the picture it says it is.
    pub fn find(&self, id: &str, hash: &str) -> Option<(PathBuf, ImageKind)> {
        if !valid_user_id(id) || !valid_avatar_hash(hash) {
            return None;
        }
        KINDS.iter().find_map(|&kind| {
            let path = self.dir.join(file_name(id, hash, kind));
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > AVATAR_MAX_BYTES as u64 {
                return None;
            }
            let mut head = [0u8; 16];
            let n = std::fs::File::open(&path).ok()?.read(&mut head).ok()?;
            (ImageKind::sniff(&head[..n]) == Some(kind)).then_some((path, kind))
        })
    }

    /// Keep a downloaded picture (re-checked: image type and size), write it atomically and delete this user's
    /// pictures with other hashes. Returns the file's path.
    pub fn store(&self, id: &str, hash: &str, img: AvatarImage) -> Result<PathBuf, String> {
        if !valid_user_id(id) || !valid_avatar_hash(hash) {
            return Err("not a safe avatar name".into());
        }
        let img = AvatarImage::check(img.bytes).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let path = self.dir.join(file_name(id, hash, img.kind));
        let tmp = self.dir.join(format!(".{}.{}.tmp", file_name(id, hash, img.kind), std::process::id()));
        std::fs::write(&tmp, &img.bytes).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.to_string());
        }
        self.prune(id, &path);
        Ok(path)
    }

    /// Delete this user's other files (older hashes, or the same hash under another type).
    fn prune(&self, id: &str, keep: &Path) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if entry.path() != keep && parse_name(name).is_some_and(|(owner, _, _)| owner == id) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Make sure the picture is on disk: use the file if it is there, else run `fetch` (the download) and keep
    /// its answer. `Err` says why there is no picture (nothing to show; the initial stays).
    pub async fn ensure<F, Fut>(&self, id: &str, hash: &str, fetch: F) -> Result<(), String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<AvatarImage>, String>>,
    {
        if self.find(id, hash).is_some() {
            return Ok(());
        }
        match fetch().await? {
            Some(img) => self.store(id, hash, img).map(|_| ()),
            None => Err("tether has no picture for this user".into()),
        }
    }

    /// Start serving (user, hash). False when the file is not (or no longer) there.
    pub fn mark_ready(&self, id: &str, hash: &str) -> bool {
        let Some((path, kind)) = self.find(id, hash) else { return false };
        self.ready.write().unwrap().insert(id.to_string(), AvatarFile { path, mime: kind.mime(), hash: hash.to_string() });
        true
    }

    pub fn is_ready(&self, id: &str, hash: &str) -> bool {
        self.ready.read().unwrap().get(id).is_some_and(|f| f.hash == hash)
    }

    /// Serve only these people (id -> hash) from now on.
    pub fn retain(&self, wanted: &HashMap<String, String>) {
        self.ready.write().unwrap().retain(|id, f| wanted.get(id).is_some_and(|h| *h == f.hash));
    }

    /// What the protocol serves for `id`, if it is in the current snapshot and cached.
    pub fn file(&self, id: &str) -> Option<AvatarFile> {
        self.ready.read().unwrap().get(id).cloned()
    }

    /// The URL for this user's picture, only once the file is cached (else `None`: draw the initial).
    pub fn url(&self, id: &str, hash: Option<&str>) -> Option<String> {
        let hash = hash?;
        self.is_ready(id, hash).then(|| avatar_url(id, hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapple_tether::{MemoryStore, Tether, TetherConfig, TetherUser};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-png-body";
    const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 body";

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("grapple-avatars-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn img(bytes: &[u8]) -> AvatarImage {
        // Built by hand so the cache's own re-check is what the tests exercise.
        AvatarImage { kind: ImageKind::sniff(bytes).unwrap_or(ImageKind::Png), bytes: bytes.to_vec() }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn stores_by_type_sniffed_from_the_bytes() {
        let dir = tmp("types");
        let c = AvatarCache::new(dir.clone());
        // the claimed kind is ignored: WebP bytes under a "png" claim are filed as webp
        let p = c.store("42", "h1", AvatarImage { kind: ImageKind::Png, bytes: WEBP.to_vec() }).unwrap();
        assert_eq!(p.file_name().unwrap(), "42-h1.webp");
        assert_eq!(c.find("42", "h1").unwrap().1, ImageKind::Webp);
        assert!(c.find("42", "other").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refuses_non_images_and_oversize() {
        let dir = tmp("refuse");
        let c = AvatarCache::new(dir.clone());
        assert!(c.store("42", "h", img(b"<html>not a picture</html>")).is_err());
        assert!(c.store("42", "h", img(b"<svg xmlns='http://www.w3.org/2000/svg'/>")).is_err());
        let mut big = PNG.to_vec();
        big.resize(AVATAR_MAX_BYTES + 1, 0);
        assert!(c.store("42", "h", img(&big)).is_err());
        let mut max = PNG.to_vec();
        max.resize(AVATAR_MAX_BYTES, 0);
        assert!(c.store("42", "h", img(&max)).is_ok());
        // unsafe names never touch the disk
        for (id, hash) in [("../x", "h"), ("42", "../../etc/passwd"), ("4/2", "h"), ("", "h"), ("42", "")] {
            assert!(c.store(id, hash, img(PNG)).is_err(), "{id} {hash}");
        }
        assert_eq!(names(&dir), vec!["42-h.png".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tampered_file_on_disk_is_not_trusted() {
        let dir = tmp("tamper");
        let c = AvatarCache::new(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("42-h.png"), b"<html>swapped</html>").unwrap();
        assert!(c.find("42", "h").is_none());
        assert!(!c.mark_ready("42", "h"));
        std::fs::write(dir.join("43-h.png"), vec![0u8; 3]).unwrap();
        assert!(c.find("43", "h").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_hash_replaces_the_old_picture_of_that_user_only() {
        let dir = tmp("replace");
        let c = AvatarCache::new(dir.clone());
        c.store("42", "old", img(PNG)).unwrap();
        c.store("7", "keep", img(PNG)).unwrap();
        c.store("42-not-an-id", "x", img(PNG)).unwrap_err(); // ids never hold a dash
        assert!(c.mark_ready("42", "old"));
        assert!(c.is_ready("42", "old"));

        c.store("42", "new", img(WEBP)).unwrap();
        assert_eq!(names(&dir), vec!["42-new.webp".to_string(), "7-keep.png".to_string()]);
        assert!(c.find("42", "old").is_none());
        // the old picture is still what is served until the new one is marked ready, and never for the new URL
        assert!(!c.is_ready("42", "new"));
        assert_eq!(c.url("42", Some("new")), None);
        assert!(c.mark_ready("42", "new"));
        assert_eq!(c.file("42").unwrap().hash, "new");
        assert_eq!(c.file("42").unwrap().mime, "image/webp");
        assert!(c.url("42", Some("new")).unwrap().ends_with("/42?v=new") || c.url("42", Some("new")).unwrap().contains("avatar/42?v=new"));
        assert_eq!(c.url("42", Some("old")), None);
        assert_eq!(c.url("42", None), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_people_in_the_snapshot_are_served() {
        let dir = tmp("retain");
        let c = AvatarCache::new(dir.clone());
        c.store("1", "a", img(PNG)).unwrap();
        c.store("2", "b", img(PNG)).unwrap();
        assert!(c.mark_ready("1", "a") && c.mark_ready("2", "b"));
        let wanted: HashMap<String, String> = [("1".to_string(), "a".to_string()), ("2".to_string(), "changed".to_string())].into();
        c.retain(&wanted);
        assert!(c.file("1").is_some());
        assert!(c.file("2").is_none(), "the hash changed: wait for the new file");
        assert!(c.file("3").is_none());
        c.retain(&HashMap::new());
        assert!(c.file("1").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn ensure_downloads_once_and_reuses_the_file() {
        let dir = tmp("ensure");
        let c = AvatarCache::new(dir.clone());
        let calls = Arc::new(AtomicUsize::new(0));
        let fetch = |bytes: &'static [u8]| {
            let calls = calls.clone();
            move || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Some(img(bytes)))
            }
        };
        c.ensure("42", "h1", fetch(PNG)).await.unwrap();
        c.ensure("42", "h1", fetch(PNG)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "second time comes from disk");
        // a fresh cache over the same folder (the next start of the app) does not download either
        let again = AvatarCache::new(dir.clone());
        again.ensure("42", "h1", fetch(PNG)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // a new hash downloads and replaces
        c.ensure("42", "h2", fetch(WEBP)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(names(&dir), vec!["42-h2.webp".to_string()]);
        // failures leave nothing behind
        assert!(c.ensure("9", "x", || async { Err("boom".to_string()) }).await.is_err());
        assert!(c.ensure("9", "x", || async { Ok(None) }).await.is_err());
        assert!(c.ensure("9", "x", || async { Ok(Some(img(b"nope"))) }).await.is_err());
        assert_eq!(names(&dir), vec!["42-h2.webp".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real download path against a LOCAL stand-in server (never a real tether): the cache fetches
    /// through `Api::avatar`, which sends no token.
    #[tokio::test]
    async fn downloads_from_a_local_server_through_the_tether_client() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", l.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = requests.clone();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let ok = req.starts_with("GET /api/v1/avatars/42/h1 ");
                seen.lock().unwrap().push(req);
                let head = if ok {
                    format!("HTTP/1.1 200 OK\r\ncontent-type: image/webp\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", WEBP.len())
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
                };
                let _ = s.write_all(head.as_bytes()).await;
                if ok {
                    let _ = s.write_all(WEBP).await;
                }
            }
        });
        let tether = Tether::new(TetherConfig::new(&origin).unwrap(), Arc::new(MemoryStore::new())).unwrap();
        let dir = tmp("local");
        let c = AvatarCache::new(dir.clone());
        let user = TetherUser { id: "42".into(), username: "rowan".into(), global_name: None, avatar: Some("h1".into()) };
        c.ensure("42", "h1", || async { tether.api().avatar(&user).await.map_err(|e| e.to_string()) }).await.unwrap();
        assert!(c.mark_ready("42", "h1"));
        assert_eq!(std::fs::read(c.file("42").unwrap().path).unwrap(), WEBP);
        let sent = requests.lock().unwrap().join("\n").to_ascii_lowercase();
        assert!(!sent.contains("authorization") && !sent.contains("cookie"), "{sent}");
        // a picture the server does not have leaves the initial
        let gone = TetherUser { avatar: Some("zz".into()), ..user };
        assert!(c.ensure("42", "zz", || async { tether.api().avatar(&gone).await.map_err(|e| e.to_string()) }).await.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn file_names_round_trip_and_strangers_are_ignored() {
        assert_eq!(parse_name("42-a_3f.webp"), Some(("42", "a_3f", ImageKind::Webp)));
        assert_eq!(parse_name("42-a-b.png"), Some(("42", "a-b", ImageKind::Png)));
        assert_eq!(parse_name(".42-h.png.123.tmp"), None);
        assert_eq!(parse_name("readme.txt"), None);
        assert_eq!(parse_name("42-h.svg"), None);
    }
}
