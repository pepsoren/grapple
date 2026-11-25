//! Store art: validated keys, URLs worked out on our side, a disk cache that keeps files forever.
//!
//! The UI asks for `grapple://store/<key>/<cover|header|hero|logo|shot{n}>`. Nothing in a request is ever
//! used to build a path or a URL: the key must match `steam:<digits>` or `epic:<32 hex>:<32 hex>`, the kind is
//! one of a fixed set, the remote URL comes from Steam's deterministic asset layout or from the offer we stored
//! ourselves (and must be on an expected host), and the file name is made of the validated key and kind only.

use crate::db::StoreDb;
use crate::epic::EpicOffer;
use crate::error::{Error, Result};
use crate::net::{Net, Svc};
use crate::steam::{self, AppInfo};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Biggest picture accepted (a 1920x1080 screenshot is about 400 KB).
const MAX_BYTES: usize = 12 * 1024 * 1024;
/// How long a 404 is remembered before asking again (seconds).
const MISS_TTL: i64 = 3 * 86_400;
pub const MAX_SHOTS: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Steam(u32),
    Epic { namespace: String, offer: String },
}

fn is_hex32(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Key {
    /// `steam:<appid>` or `epic:<namespace>:<offer id>`, nothing else.
    pub fn parse(key: &str) -> Option<Key> {
        let mut it = key.split(':');
        match (it.next()?, it.next(), it.next(), it.next()) {
            ("steam", Some(id), None, None) if !id.is_empty() && id.len() <= 10 && id.bytes().all(|b| b.is_ascii_digit()) => id.parse().ok().filter(|n| *n > 0).map(Key::Steam),
            ("epic", Some(ns), Some(offer), None) if is_hex32(ns) && is_hex32(offer) => Some(Key::Epic { namespace: ns.into(), offer: offer.into() }),
            _ => None,
        }
    }

    pub fn as_string(&self) -> String {
        match self {
            Key::Steam(id) => format!("steam:{id}"),
            Key::Epic { namespace, offer } => format!("epic:{namespace}:{offer}"),
        }
    }

    /// A string safe in a file name.
    fn file_stem(&self) -> String {
        self.as_string().replace(':', "-")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cover,
    Header,
    Hero,
    Logo,
    Shot(u8),
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "cover" => Kind::Cover,
            "header" => Kind::Header,
            "hero" => Kind::Hero,
            "logo" => Kind::Logo,
            _ => {
                let n = s.strip_prefix("shot")?;
                if n.is_empty() || n.len() > 2 || !n.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let n: u8 = n.parse().ok()?;
                if n as usize >= MAX_SHOTS {
                    return None;
                }
                Kind::Shot(n)
            }
        })
    }

    pub fn name(self) -> String {
        match self {
            Kind::Cover => "cover".into(),
            Kind::Header => "header".into(),
            Kind::Hero => "hero".into(),
            Kind::Logo => "logo".into(),
            Kind::Shot(n) => format!("shot{n}"),
        }
    }
}

/// The URL the UI uses for a picture (Linux/macOS `grapple://store/<key>/<kind>`, Windows
/// `http://grapple.localhost/store/<key>/<kind>`). The key is percent-encoded like the library's art URLs.
pub fn art_url(key: &str, kind: &str, windows: bool) -> String {
    let k = key.replace(':', "%3A");
    if windows { format!("http://grapple.localhost/store/{k}/{kind}") } else { format!("grapple://store/{k}/{kind}") }
}

/// The remote files to try for a Steam app and kind, in order.
fn steam_files(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Cover => &["library_600x900.jpg", "header.jpg"],
        Kind::Header => &["header.jpg"],
        Kind::Hero => &["library_hero.jpg", "header.jpg"],
        Kind::Logo => &["logo.png"],
        Kind::Shot(_) => &[],
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?;
    host.rsplit('@').next()?.split(':').next()
}

/// Whether a remote picture may come from this URL: Steam's or Epic's CDNs (and, in tests, the configured one).
fn allowed(url: &str, cdn_base: &str) -> bool {
    let Some(h) = host_of(url) else { return false };
    let h = h.to_ascii_lowercase();
    let ends = |d: &str| h == d || h.ends_with(&format!(".{d}"));
    ends("steamstatic.com") || ends("epicgames.com") || ends("unrealengine.com") || host_of(cdn_base).is_some_and(|b| b.eq_ignore_ascii_case(&h))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtFile {
    pub path: PathBuf,
    pub mime: &'static str,
    pub len: u64,
    pub mtime: u64,
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

pub struct ArtCache {
    dir: PathBuf,
    db: StoreDb,
    net: Arc<Net>,
    cc: Arc<dyn Fn() -> String + Send + Sync>,
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ArtCache {
    pub fn new(dir: PathBuf, db: StoreDb, net: Arc<Net>, cc: Arc<dyn Fn() -> String + Send + Sync>) -> ArtCache {
        ArtCache { dir, db, net, cc, inflight: Mutex::new(HashMap::new()) }
    }

    fn file(&self, key: &Key, kind: Kind) -> PathBuf {
        self.dir.join(format!("{}_{}", key.file_stem(), kind.name()))
    }

    async fn on_disk(path: &Path) -> Option<ArtFile> {
        let meta = tokio::fs::metadata(path).await.ok()?;
        if !meta.is_file() || meta.len() == 0 {
            return None;
        }
        let mut head = [0u8; 16];
        let mut f = tokio::fs::File::open(path).await.ok()?;
        let n = tokio::io::AsyncReadExt::read(&mut f, &mut head).await.ok()?;
        let mime = sniff(&head[..n])?;
        let mtime = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(ArtFile { path: path.to_path_buf(), mime, len: meta.len(), mtime })
    }

    /// The remote URLs to try for a picture.
    fn candidates(&self, key: &Key, kind: Kind) -> Vec<String> {
        match key {
            Key::Steam(id) => {
                if let Kind::Shot(n) = kind {
                    let cc = (self.cc)();
                    let shots = self.db.infos(&cc, &[*id]).ok().and_then(|mut m| m.remove(id)).and_then(|(i, _)| i).map(|i: AppInfo| i.shots).unwrap_or_default();
                    return shots.get(n as usize).cloned().into_iter().collect();
                }
                steam_files(kind).iter().map(|f| steam::art_url(&self.net.endpoints.steam_cdn, *id, f)).collect()
            }
            Key::Epic { .. } => {
                let Some(o) = self.db.epic_offer(&key.as_string()).ok().flatten().and_then(|(j, _)| serde_json::from_str::<EpicOffer>(&j).ok()) else { return Vec::new() };
                let pick = |v: &[&Option<String>]| v.iter().filter_map(|x| x.as_deref()).map(str::to_string).collect::<Vec<_>>();
                match kind {
                    Kind::Cover => pick(&[&o.cover, &o.thumb, &o.wide]),
                    Kind::Header | Kind::Hero => pick(&[&o.wide, &o.cover]),
                    Kind::Logo | Kind::Shot(_) => Vec::new(),
                }
            }
        }
    }

    /// The picture, from disk or fetched now. `Ok(None)`: the store has no such picture.
    pub async fn get(&self, key_str: &str, kind_str: &str) -> Result<Option<ArtFile>> {
        let key = Key::parse(key_str).ok_or_else(|| Error::Invalid("bad art key".into()))?;
        let kind = Kind::parse(kind_str).ok_or_else(|| Error::Invalid("bad art kind".into()))?;
        let path = self.file(&key, kind);
        if let Some(f) = Self::on_disk(&path).await {
            return Ok(Some(f));
        }
        let stem = key.as_string();
        let kname = kind.name();
        if self.db.art_missing(&stem, &kname, MISS_TTL).unwrap_or(false) {
            return Ok(None);
        }
        // One download per file at a time.
        let lock = {
            let mut m = self.inflight.lock().await;
            m.entry(path.to_string_lossy().into_owned()).or_default().clone()
        };
        let _guard = lock.lock().await;
        if let Some(f) = Self::on_disk(&path).await {
            return Ok(Some(f));
        }
        let urls = self.candidates(&key, kind);
        if urls.is_empty() {
            return Ok(None); // nothing known yet (details not fetched): not remembered as a miss
        }
        for url in urls {
            if !allowed(&url, &self.net.endpoints.steam_cdn) {
                continue;
            }
            let r = match self.net.get(Svc::Cdn, &url, &[]).await {
                Ok(r) => r,
                Err(e) => return Err(e),
            };
            if r.status == 404 || r.status == 403 || r.status == 410 {
                continue;
            }
            if r.status != 200 {
                return Err(Error::Status(r.status));
            }
            if r.body.len() > MAX_BYTES || sniff(&r.body).is_none() {
                continue;
            }
            tokio::fs::create_dir_all(&self.dir).await?;
            let tmp = path.with_extension("part");
            tokio::fs::write(&tmp, &r.body).await?;
            tokio::fs::rename(&tmp, &path).await?;
            return Ok(Self::on_disk(&path).await);
        }
        let _ = self.db.set_art_missing(&stem, &kname);
        Ok(None)
    }

    /// Whether the picture is on disk already (no network).
    pub async fn cached(&self, key_str: &str, kind_str: &str) -> bool {
        match (Key::parse(key_str), Kind::parse(kind_str)) {
            (Some(k), Some(kind)) => Self::on_disk(&self.file(&k, kind)).await.is_some(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_validated() {
        assert_eq!(Key::parse("steam:1621690"), Some(Key::Steam(1621690)));
        let ns = "a".repeat(32);
        let of = "0123456789abcdef0123456789abcdef";
        assert_eq!(Key::parse(&format!("epic:{ns}:{of}")), Some(Key::Epic { namespace: ns.clone(), offer: of.into() }));
        for bad in [
            "", "steam:", "steam:0", "steam:../../etc", "steam:12/3", "steam:12:3", "steam:-1", "steam:12345678901", "steam: 1", "STEAM:1",
            "epic:abc:def", "epic:a:b:c", "other:1", "steam:1\0", "steam:1%2F", &format!("epic:{}:{}", "A".repeat(32), of), &format!("epic:{ns}"),
        ] {
            assert_eq!(Key::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn kinds_are_a_fixed_set() {
        assert_eq!(Kind::parse("cover"), Some(Kind::Cover));
        assert_eq!(Kind::parse("shot3"), Some(Kind::Shot(3)));
        assert_eq!(Kind::parse("shot11"), Some(Kind::Shot(11)));
        for bad in ["", "shot", "shot12", "shot99", "shot-1", "shot1a", "Cover", "../cover", "cover/", "icon", "shot001"] {
            assert_eq!(Kind::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn urls_for_both_platform_forms() {
        assert_eq!(art_url("steam:730", "cover", false), "grapple://store/steam%3A730/cover");
        assert_eq!(art_url("steam:730", "shot2", true), "http://grapple.localhost/store/steam%3A730/shot2");
    }

    #[test]
    fn only_known_cdns_are_fetched() {
        let cdn = "http://127.0.0.1:9";
        assert!(allowed("https://shared.akamai.steamstatic.com/a.jpg", cdn));
        assert!(allowed("https://cdn1.epicgames.com/offer/x.jpg", cdn));
        assert!(allowed("https://cdn2.unrealengine.com/x.jpg", cdn));
        assert!(allowed("http://127.0.0.1:9/store_item_assets/x.jpg", cdn));
        assert!(!allowed("https://evil.example/steamstatic.com/a.jpg", cdn));
        assert!(!allowed("https://steamstatic.com.evil.example/a.jpg", cdn));
        assert!(!allowed("https://user@evil.example/a.jpg", cdn));
        assert!(!allowed("file:///etc/passwd", cdn));
    }

    #[test]
    fn sniffing_needs_a_real_image() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), Some("image/png"));
        assert_eq!(sniff(b"<html>nope"), None);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    }
}
