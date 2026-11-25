//! `grapple://art/<game-id>/<cover|hero|logo|icon>`: streams a game's art straight from the store's own
//! cache. Linux/macOS URL: `grapple://art/<id>/<kind>`; Windows: `http://grapple.localhost/art/<id>/<kind>`.
//!
//! Safety: the id is validated, then looked up in the index of paths the scanner returned. A path is never
//! built from the request, so there is nothing to traverse. Anything else is a 404.
//!
//! The same scheme serves the store's pictures: `grapple://store/<key>/<cover|header|hero|logo|shot{n}>`
//! (`steam:<appid>` or `epic:<namespace>:<offer>` percent-encoded; Windows:
//! `http://grapple.localhost/store/<key>/<kind>`). The key and kind are validated by grapple-store, which
//! picks the remote file itself and keeps it on disk; nothing in the request is used as a path or a URL.
//!
//! The same scheme serves tether profile pictures: `grapple://avatar/<user id>?v=<hash>` (Windows:
//! `http://grapple.localhost/avatar/<user id>?v=<hash>`). Only people in the current tether snapshot whose
//! picture is already in the cache have a file; the hash in `?v=` makes a changed picture a new URL.

use crate::AppState;
use crate::backend::{ArtKind, valid_game_id, valid_user_id};
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;
use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// (game id, kind) from a request URI.
fn parse(uri: &tauri::http::Uri) -> Option<(String, ArtKind)> {
    let mut segs: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs.first() == Some(&"art") && segs.len() == 3 {
        segs.remove(0); // Windows form: the host is grapple.localhost, "art" is the first path segment
    }
    if segs.len() != 2 {
        return None;
    }
    let id = percent_decode(segs[0])?;
    if !valid_game_id(&id) {
        return None;
    }
    Some((id, ArtKind::parse(segs[1])?))
}

/// (key, kind) from a store-art request URI, in every URL form. Checked before [`parse`]: `store` is also a
/// valid first segment there, and `steam:1` a valid id.
fn parse_store(uri: &tauri::http::Uri) -> Option<(String, String)> {
    let mut segs: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    if uri.host() == Some("store") && segs.len() == 2 {
        // Linux/macOS: grapple://store/<key>/<kind>
    } else if segs.len() == 3 && segs[0] == "store" {
        segs.remove(0); // Windows form (host grapple.localhost), or grapple://localhost/store/<key>/<kind>
    } else {
        return None;
    }
    Some((percent_decode(segs[0])?, segs[1].to_string()))
}

/// (user id, `?v=` value) from an avatar request URI.
fn parse_avatar(uri: &tauri::http::Uri) -> Option<(String, Option<String>)> {
    let mut segs: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    if uri.host() == Some("avatar") && segs.len() == 1 {
        // Linux/macOS: grapple://avatar/<id>
    } else if segs.len() == 2 && segs[0] == "avatar" {
        segs.remove(0); // Windows form (host grapple.localhost), or grapple://localhost/avatar/<id>
    } else {
        return None;
    }
    let id = percent_decode(segs[0])?;
    if !valid_user_id(&id) {
        return None;
    }
    let v = uri.query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("v=")).map(str::to_string));
    Some((id, v))
}

fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(code).header(header::CONTENT_TYPE, "text/plain").body(Vec::new()).unwrap()
}

pub fn handle<R: tauri::Runtime>(ctx: UriSchemeContext<'_, R>, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    tauri::async_runtime::spawn(async move {
        responder.respond(serve(&app, &req).await);
    });
}

/// A tether profile picture, from the cache the tether service keeps (never from a path in the request).
async fn serve_avatar<R: tauri::Runtime>(app: &tauri::AppHandle<R>, req: &Request<Vec<u8>>, id: &str, v: Option<&str>) -> Response<Vec<u8>> {
    let Some(state) = app.try_state::<Arc<AppState>>() else { return status(StatusCode::SERVICE_UNAVAILABLE) };
    let Some(file) = state.backend.avatar(id) else { return status(StatusCode::NOT_FOUND) };
    // The URL names its picture: when it is the current one the answer can be kept for good; a bare or stale URL
    // is always asked again.
    let cache = if v == Some(file.hash.as_str()) { "public, max-age=31536000, immutable" } else { "no-cache" };
    let etag = format!("\"{}\"", file.hash);
    if req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, cache)
            .body(Vec::new())
            .unwrap();
    }
    match tokio::fs::read(&file.path).await {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, file.mime)
            .header(header::CONTENT_LENGTH, bytes.len())
            .header(header::CACHE_CONTROL, cache)
            .header(header::ETAG, etag)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header("X-Content-Type-Options", "nosniff")
            .body(bytes)
            .unwrap(),
        Err(_) => status(StatusCode::NOT_FOUND),
    }
}

/// A store picture: the grapple-store crate resolves, downloads once and caches it on disk.
async fn serve_store<R: tauri::Runtime>(app: &tauri::AppHandle<R>, req: &Request<Vec<u8>>, key: &str, kind: &str) -> Response<Vec<u8>> {
    use crate::store::Served;
    match crate::store::art(app, key, kind).await {
        Served::Bad => status(StatusCode::NOT_FOUND),
        // The network failed or the store is not up yet: not remembered, the page asks again later.
        Served::Unavailable => Response::builder().status(StatusCode::SERVICE_UNAVAILABLE).header(header::CACHE_CONTROL, "no-store").body(Vec::new()).unwrap(),
        Served::Missing => Response::builder().status(StatusCode::NOT_FOUND).header(header::CACHE_CONTROL, "public, max-age=3600").header(header::CONTENT_TYPE, "text/plain").body(Vec::new()).unwrap(),
        Served::File { path, mime, mtime, len } => {
            let etag = format!("\"{mtime:x}-{len:x}\"");
            // Art on disk is kept for good; a day in the webview's cache is plenty.
            let cache = "public, max-age=86400";
            if req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
                return Response::builder().status(StatusCode::NOT_MODIFIED).header(header::ETAG, etag).header(header::CACHE_CONTROL, cache).body(Vec::new()).unwrap();
            }
            match tokio::fs::read(&path).await {
                Ok(bytes) => Response::builder()
                    .header(header::CONTENT_TYPE, mime)
                    .header(header::CONTENT_LENGTH, bytes.len())
                    .header(header::CACHE_CONTROL, cache)
                    .header(header::ETAG, etag)
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .header("X-Content-Type-Options", "nosniff")
                    .body(bytes)
                    .unwrap(),
                Err(_) => status(StatusCode::NOT_FOUND),
            }
        }
    }
}

async fn serve<R: tauri::Runtime>(app: &tauri::AppHandle<R>, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if let Some((key, kind)) = parse_store(req.uri()) {
        return serve_store(app, req, &key, &kind).await;
    }
    if let Some((id, v)) = parse_avatar(req.uri()) {
        return serve_avatar(app, req, &id, v.as_deref()).await;
    }
    let Some((id, kind)) = parse(req.uri()) else { return status(StatusCode::NOT_FOUND) };
    let Some(state) = app.try_state::<Arc<AppState>>() else { return status(StatusCode::SERVICE_UNAVAILABLE) };
    let Some(path) = state.backend.art.path(&id, kind) else { return status(StatusCode::NOT_FOUND) };
    let Ok(meta) = tokio::fs::metadata(&path).await else { return status(StatusCode::NOT_FOUND) };
    if !meta.is_file() {
        return status(StatusCode::NOT_FOUND);
    }
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    let etag = format!("\"{mtime:x}-{:x}\"", meta.len());
    let cache = "public, max-age=31536000, immutable"; // the URL carries ?v=<art mtime>, so a change is a new URL
    if req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, cache)
            .body(Vec::new())
            .unwrap();
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, content_type(&path))
            .header(header::CONTENT_LENGTH, bytes.len())
            .header(header::CACHE_CONTROL, cache)
            .header(header::ETAG, etag)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .header("X-Content-Type-Options", "nosniff")
            .body(bytes)
            .unwrap(),
        Err(_) => status(StatusCode::NOT_FOUND),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<(String, ArtKind)> {
        parse(&s.parse().unwrap())
    }

    #[test]
    fn parses_both_url_forms() {
        assert_eq!(p("grapple://art/steam%3A730/cover?v=1"), Some(("steam:730".into(), ArtKind::Cover)));
        assert_eq!(p("http://grapple.localhost/art/epic%3Asugar/hero"), Some(("epic:sugar".into(), ArtKind::Hero)));
        assert_eq!(p("grapple://localhost/steam:730/icon"), Some(("steam:730".into(), ArtKind::Icon)));
    }

    fn a(s: &str) -> Option<(String, Option<String>)> {
        parse_avatar(&s.parse().unwrap())
    }

    #[test]
    fn parses_avatar_urls_in_every_form() {
        assert_eq!(a("grapple://avatar/1234567890?v=abc_1"), Some(("1234567890".into(), Some("abc_1".into()))));
        assert_eq!(a("grapple://avatar/42"), Some(("42".into(), None)));
        assert_eq!(a("http://grapple.localhost/avatar/42?v=h"), Some(("42".into(), Some("h".into()))));
        assert_eq!(a("grapple://localhost/avatar/42?x=1&v=h"), Some(("42".into(), Some("h".into()))));
        // art URLs are not avatars, avatars are not art
        assert_eq!(a("grapple://art/steam%3A730/cover"), None);
        assert_eq!(p("grapple://avatar/42"), None);
        assert_eq!(p("http://grapple.localhost/avatar/42"), None);
    }

    #[test]
    fn rejects_bad_avatar_ids() {
        for bad in [
            "grapple://avatar/",
            "grapple://avatar/..%2F..%2Fetc%2Fpasswd",
            "grapple://avatar/%2e%2e",
            "grapple://avatar/4%2F2",
            "grapple://avatar/a-b",
            "grapple://avatar/a.b",
            "grapple://avatar/steam%3A730",
            "grapple://avatar/42/extra",
            "grapple://avatar/%zz",
            "http://grapple.localhost/avatar/",
            "http://grapple.localhost/avatar/42/extra",
        ] {
            assert_eq!(a(bad), None, "{bad}");
        }
        assert_eq!(a(&format!("grapple://avatar/{}", "1".repeat(65))), None);
    }

    fn st(s: &str) -> Option<(String, String)> {
        parse_store(&s.parse().unwrap())
    }

    #[test]
    fn parses_store_art_urls_in_every_form() {
        assert_eq!(st("grapple://store/steam%3A1621690/cover"), Some(("steam:1621690".into(), "cover".into())));
        assert_eq!(st("grapple://store/steam:1621690/shot3?v=1"), Some(("steam:1621690".into(), "shot3".into())));
        assert_eq!(st("http://grapple.localhost/store/epic%3Aabc%3Adef/hero"), Some(("epic:abc:def".into(), "hero".into())));
        assert_eq!(st("grapple://localhost/store/steam%3A1/logo"), Some(("steam:1".into(), "logo".into())));
        // library art and avatars are not store art
        assert_eq!(st("grapple://art/steam%3A730/cover"), None);
        assert_eq!(st("grapple://avatar/42"), None);
        assert_eq!(st("grapple://store/steam%3A1"), None);
        assert_eq!(st("grapple://store/steam%3A1/cover/extra"), None);
        assert_eq!(st("grapple://store/%zz/cover"), None);
        // and the store host must never reach the library art lookup by accident
        assert_eq!(p("grapple://store/steam%3A1/cover").map(|(id, _)| id), Some("steam:1".to_string()), "parse() alone cannot tell: serve() checks the store form first");
    }

    #[test]
    fn rejects_bad_ids_and_kinds() {
        assert_eq!(p("grapple://art/..%2F..%2Fetc%2Fpasswd/cover"), None);
        assert_eq!(p("grapple://art/steam%3A../cover"), None);
        assert_eq!(p("grapple://art/steam%3A730/passwd"), None);
        assert_eq!(p("grapple://art/other%3A1/cover"), None);
        assert_eq!(p("grapple://art/steam%3A730"), None);
    }
}
