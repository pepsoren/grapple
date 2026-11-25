//! Profile pictures. They are public (`GET <origin>/api/v1/avatars/<user id>/<hash>`), so the request carries
//! no token and no cookie; it goes only to the checked instance origin ([`crate::TetherConfig`]: https, or
//! plain http on this computer or a tailnet), never follows a redirect, stops reading at
//! [`AVATAR_MAX_BYTES`], and the answer must really be a PNG, JPEG, WebP or GIF (decided from its bytes).

use crate::api::Api;
use crate::error::{Error, Result};
use crate::model::TetherUser;
use std::time::Duration;

/// The biggest picture Grapple keeps. tether stores avatars as small square WebP, far below this.
pub const AVATAR_MAX_BYTES: usize = 2 * 1024 * 1024;

const AVATAR_TIMEOUT: Duration = Duration::from_secs(15);

/// The raster formats Grapple shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageKind {
    /// What the bytes are, from their magic numbers (a claimed content type or a file name is never trusted).
    pub fn sniff(bytes: &[u8]) -> Option<ImageKind> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(ImageKind::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(ImageKind::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(ImageKind::Gif)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(ImageKind::Webp)
        } else {
            None
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            ImageKind::Png => "png",
            ImageKind::Jpeg => "jpg",
            ImageKind::Webp => "webp",
            ImageKind::Gif => "gif",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            ImageKind::Png => "image/png",
            ImageKind::Jpeg => "image/jpeg",
            ImageKind::Webp => "image/webp",
            ImageKind::Gif => "image/gif",
        }
    }

    pub fn from_ext(ext: &str) -> Option<ImageKind> {
        Some(match ext {
            "png" => ImageKind::Png,
            "jpg" => ImageKind::Jpeg,
            "webp" => ImageKind::Webp,
            "gif" => ImageKind::Gif,
            _ => return None,
        })
    }
}

/// A downloaded, checked picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvatarImage {
    pub kind: ImageKind,
    pub bytes: Vec<u8>,
}

impl AvatarImage {
    /// Accept `bytes` only when they are within [`AVATAR_MAX_BYTES`] and a known image format.
    pub fn check(bytes: Vec<u8>) -> Result<AvatarImage> {
        if bytes.len() > AVATAR_MAX_BYTES {
            return Err(Error::Avatar(format!("{} bytes is over the {AVATAR_MAX_BYTES} byte limit", bytes.len())));
        }
        let kind = ImageKind::sniff(&bytes).ok_or_else(|| Error::Avatar("not a PNG, JPEG, WebP or GIF picture".into()))?;
        Ok(AvatarImage { kind, bytes })
    }
}

fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// A user id as it may appear in a path and in a file name: letters, digits and `_` (tether's are decimal).
/// No `-`, so `<id>-<hash>` always splits at the first dash.
pub fn valid_user_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(plain)
}

/// An avatar hash as it may appear in a path and in a file name.
pub fn valid_avatar_hash(hash: &str) -> bool {
    (1..=64).contains(&hash.len()) && hash.bytes().all(|b| plain(b) || b == b'-')
}

impl Api {
    /// The user's current picture, or `None` when they have none (or tether has lost it: `404`).
    /// Needs no sign-in and sends no token.
    pub async fn avatar(&self, user: &TetherUser) -> Result<Option<AvatarImage>> {
        let Some(hash) = user.avatar.as_deref() else { return Ok(None) };
        if !valid_user_id(&user.id) || !valid_avatar_hash(hash) {
            return Err(Error::Protocol("an avatar id or hash that is not safe to use in a path".into()));
        }
        let url = self.config().api(&format!("/avatars/{}/{hash}", user.id));
        let mut res = self.public_http().get(url).timeout(AVATAR_TIMEOUT).send().await?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(Error::Status(res.status().as_u16()));
        }
        if let Some(ct) = res.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
            let ct = ct.trim().to_ascii_lowercase();
            if !(ct.starts_with("image/") || ct.starts_with("application/octet-stream")) {
                return Err(Error::Avatar(format!("tether answered {ct}, not a picture")));
            }
        }
        if res.content_length().is_some_and(|n| n > AVATAR_MAX_BYTES as u64) {
            return Err(Error::Avatar("the picture is over the size limit".into()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await? {
            if body.len() + chunk.len() > AVATAR_MAX_BYTES {
                return Err(Error::Avatar("the picture is over the size limit".into()));
            }
            body.extend_from_slice(&chunk);
        }
        AvatarImage::check(body).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MemoryStore, Tether, TetherConfig};
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::routing::get;
    use std::sync::{Arc, Mutex};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest-of-a-png";
    const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 data";

    #[test]
    fn types_come_from_the_bytes() {
        assert_eq!(ImageKind::sniff(PNG), Some(ImageKind::Png));
        assert_eq!(ImageKind::sniff(&[0xff, 0xd8, 0xff, 0xe0, 0, 0x10]), Some(ImageKind::Jpeg));
        assert_eq!(ImageKind::sniff(b"GIF89a\x01\x00"), Some(ImageKind::Gif));
        assert_eq!(ImageKind::sniff(b"GIF87a"), Some(ImageKind::Gif));
        assert_eq!(ImageKind::sniff(WEBP), Some(ImageKind::Webp));
        // look-alikes and non-images
        assert_eq!(ImageKind::sniff(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        assert_eq!(ImageKind::sniff(b"<svg xmlns='http://www.w3.org/2000/svg'/>"), None);
        assert_eq!(ImageKind::sniff(b"<!doctype html><title>x</title>"), None);
        assert_eq!(ImageKind::sniff(b"MZ\x90\x00"), None);
        assert_eq!(ImageKind::sniff(b""), None);
        assert_eq!(ImageKind::sniff(b"\x89PNG"), None);
        for k in [ImageKind::Png, ImageKind::Jpeg, ImageKind::Webp, ImageKind::Gif] {
            assert_eq!(ImageKind::from_ext(k.ext()), Some(k));
            assert!(k.mime().starts_with("image/"));
        }
        assert_eq!(ImageKind::from_ext("svg"), None);
    }

    #[test]
    fn check_enforces_type_and_size() {
        assert_eq!(AvatarImage::check(PNG.to_vec()).unwrap().kind, ImageKind::Png);
        assert!(AvatarImage::check(b"hello".to_vec()).is_err());
        let mut big = PNG.to_vec();
        big.resize(AVATAR_MAX_BYTES, 0);
        assert!(AvatarImage::check(big.clone()).is_ok(), "exactly at the limit is fine");
        big.push(0);
        assert!(AvatarImage::check(big).is_err());
    }

    #[test]
    fn ids_and_hashes_are_path_safe() {
        assert!(valid_user_id("1234567890123456789"));
        assert!(valid_user_id("rowan_2"));
        for bad in ["", "a-b", "../x", "a/b", "a b", "a.b", "%2e", &"1".repeat(65)] {
            assert!(!valid_user_id(bad), "{bad}");
        }
        assert!(valid_avatar_hash("a_3f9c2b"));
        assert!(valid_avatar_hash("3f9c-2b"));
        for bad in ["", "../x", "a/b", "a.b", "a?b", "a#b", &"f".repeat(65)] {
            assert!(!valid_avatar_hash(bad), "{bad}");
        }
    }

    /// A local server (never a real tether) answering `/api/v1/avatars/:id/:hash` with `answer`, and
    /// recording the request headers it saw.
    async fn serve(answer: fn() -> (StatusCode, Vec<(&'static str, String)>, Vec<u8>)) -> (Tether, Arc<Mutex<Vec<HeaderMap>>>) {
        let seen: Arc<Mutex<Vec<HeaderMap>>> = Arc::default();
        let sink = seen.clone();
        let app = Router::new().route(
            "/api/v1/avatars/{id}/{hash}",
            get(move |headers: HeaderMap| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(headers);
                    let (status, hdrs, body) = answer();
                    let mut map = HeaderMap::new();
                    for (k, v) in hdrs {
                        map.insert(header::HeaderName::from_static(k), v.parse().unwrap());
                    }
                    (status, map, body)
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let tether = Tether::new(TetherConfig::new(&origin).unwrap(), Arc::new(MemoryStore::new())).unwrap();
        (tether, seen)
    }

    fn user(avatar: Option<&str>) -> TetherUser {
        TetherUser { id: "42".into(), username: "rowan".into(), global_name: None, avatar: avatar.map(Into::into) }
    }

    #[tokio::test]
    async fn downloads_a_picture_without_a_token() {
        let (t, seen) = serve(|| (StatusCode::OK, vec![("content-type", "image/webp".into())], WEBP.to_vec())).await;
        let img = t.api().avatar(&user(Some("abc123"))).await.unwrap().unwrap();
        assert_eq!((img.kind, img.bytes.as_slice()), (ImageKind::Webp, WEBP));
        let headers = seen.lock().unwrap();
        assert_eq!(headers.len(), 1);
        assert!(!headers[0].contains_key(header::AUTHORIZATION) && !headers[0].contains_key(header::COOKIE));
    }

    #[tokio::test]
    async fn no_avatar_is_no_request() {
        let (t, seen) = serve(|| (StatusCode::OK, vec![], WEBP.to_vec())).await;
        assert_eq!(t.api().avatar(&user(None)).await.unwrap(), None);
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refuses_what_is_not_a_small_picture() {
        // HTML under an image content type: the bytes decide
        let (t, _) = serve(|| (StatusCode::OK, vec![("content-type", "image/png".into())], b"<html>hi</html>".to_vec())).await;
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Avatar(_))));
        // a declared non-image type, even with PNG bytes
        let (t, _) = serve(|| (StatusCode::OK, vec![("content-type", "text/html".into())], PNG.to_vec())).await;
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Avatar(_))));
        // too big (Content-Length known up front)
        let (t, _) = serve(|| {
            let mut body = PNG.to_vec();
            body.resize(AVATAR_MAX_BYTES + 1, 0);
            (StatusCode::OK, vec![("content-type", "image/png".into())], body)
        })
        .await;
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Avatar(_))));
        // errors and a missing picture
        let (t, _) = serve(|| (StatusCode::INTERNAL_SERVER_ERROR, vec![], vec![])).await;
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Status(500))));
        let (t, _) = serve(|| (StatusCode::NOT_FOUND, vec![], vec![])).await;
        assert_eq!(t.api().avatar(&user(Some("h"))).await.unwrap(), None);
        // a redirect is not followed
        let (t, _) = serve(|| (StatusCode::FOUND, vec![("location", "http://127.0.0.1:1/x".into())], vec![])).await;
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Status(302))));
        // an unsafe hash never reaches the network
        let (t, seen) = serve(|| (StatusCode::OK, vec![], PNG.to_vec())).await;
        assert!(t.api().avatar(&user(Some("../../x"))).await.is_err());
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_body_that_grows_past_the_limit_without_a_length_is_cut_off() {
        // chunked, no Content-Length: the streaming check must stop it
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            while let Ok((mut s, _)) = l.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: image/png\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await;
                let chunk = vec![0u8; 256 * 1024];
                for i in 0..16 {
                    let head = if i == 0 { PNG.to_vec() } else { Vec::new() };
                    let payload = [head, chunk.clone()].concat();
                    if s.write_all(format!("{:x}\r\n", payload.len()).as_bytes()).await.is_err()
                        || s.write_all(&payload).await.is_err()
                        || s.write_all(b"\r\n").await.is_err()
                    {
                        return;
                    }
                }
                let _ = s.write_all(b"0\r\n\r\n").await;
            }
        });
        let t = Tether::new(TetherConfig::new(&origin).unwrap(), Arc::new(MemoryStore::new())).unwrap();
        assert!(matches!(t.api().avatar(&user(Some("h"))).await, Err(Error::Avatar(_))));
    }
}
