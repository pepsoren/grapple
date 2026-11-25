//! Before the browser opens: can this tether sign apps in at all?
//!
//! One GET of the authorize route with no session and no token. A tether with app sign-in answers
//! it (401: "sign in to tether first", or a page); one without the feature answers 404, so the
//! browser would only show "not found". Nothing secret is sent: no token, no code, no `state`.

use crate::config::TetherConfig;
use std::time::Duration;

/// What [`probe`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// The authorize route exists: go ahead and open the browser.
    Ok,
    /// tether answered, but has no app sign-in (an older tether).
    Unsupported,
    /// No answer, or the server is failing (network error, 5xx, an unexpected redirect).
    Unreachable,
}

/// The decision, from the status code (`None`: no answer at all).
pub fn classify(status: Option<u16>) -> Probe {
    match status {
        None => Probe::Unreachable,
        Some(404 | 405 | 410 | 501) => Probe::Unsupported,
        Some(300..=399) | Some(500..=599) => Probe::Unreachable,
        Some(_) => Probe::Ok,
    }
}

/// Ask `config`'s tether (one GET, 8 s at most).
pub async fn probe(http: &reqwest::Client, config: &TetherConfig) -> Probe {
    let answer = http
        .get(config.api("/oauth2/authorize"))
        .timeout(Duration::from_secs(8))
        .send()
        .await;
    classify(answer.ok().map(|r| r.status().as_u16()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn statuses() {
        assert_eq!(classify(None), Probe::Unreachable);
        assert_eq!(classify(Some(401)), Probe::Ok);
        assert_eq!(classify(Some(400)), Probe::Ok);
        assert_eq!(classify(Some(200)), Probe::Ok);
        assert_eq!(classify(Some(404)), Probe::Unsupported);
        assert_eq!(classify(Some(405)), Probe::Unsupported);
        assert_eq!(classify(Some(302)), Probe::Unreachable);
        assert_eq!(classify(Some(502)), Probe::Unreachable);
    }

    /// A one-request HTTP server answering `status`.
    async fn serve_once(status: &'static str) -> String {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = l.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let body = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
                let _ = s.write_all(body.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn against_a_server() {
        let http = reqwest::Client::new();
        let ok = TetherConfig::new(&serve_once("401 Unauthorized").await).unwrap();
        assert_eq!(probe(&http, &ok).await, Probe::Ok);
        let old = TetherConfig::new(&serve_once("404 Not Found").await).unwrap();
        assert_eq!(probe(&http, &old).await, Probe::Unsupported);
        // A port nobody listens on.
        let gone = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            TetherConfig::new(&format!("http://{a}")).unwrap()
        };
        assert_eq!(probe(&http, &gone).await, Probe::Unreachable);
    }
}
