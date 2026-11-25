//! The sign-in flow of `docs/tether-contract.md` section 2: PKCE (S256), a random `state`, a
//! listener on `127.0.0.1:<free port>`, the system browser, and the token endpoint.

use crate::config::TetherConfig;
use crate::error::{Error, Result};
use crate::secret::{Secret, ct_eq};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use url::Url;

/// The client id tether knows Grapple by.
pub const CLIENT_ID: &str = "grapple";
/// What Grapple asks for (`invites`: game invites, contract section 7).
pub const SCOPES: &str = "identify presence.game friends.read invites";
/// What Grapple asks a tether without game invites for (it refuses a request that names `invites`).
pub const SCOPES_WITHOUT_INVITES: &str = "identify presence.game friends.read";
/// How long the browser has to come back.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// `n` random bytes, base64url without padding.
pub fn random_b64(n: usize) -> Secret {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).expect("the system random source failed");
    let out = URL_SAFE_NO_PAD.encode(&buf);
    buf.fill(0);
    Secret::new(out)
}

/// A PKCE pair (RFC 7636, S256).
pub struct Pkce {
    pub verifier: Secret,
    pub challenge: String,
}

pub fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn pkce() -> Pkce {
    let verifier = random_b64(32);
    let challenge = s256(verifier.expose());
    Pkce {
        verifier,
        challenge,
    }
}

/// The page to open in the browser, asking for `scopes` ([`SCOPES`] or [`SCOPES_WITHOUT_INVITES`]).
pub fn authorize_url(
    config: &TetherConfig,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    scopes: &str,
) -> Url {
    let mut url = Url::parse(&format!("{}/oauth2/authorize", config.origin()))
        .expect("an origin plus a path is a URL");
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", scopes)
        .append_pair("state", state)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256");
    url
}

/// Opens a URL in the person's browser.
pub trait Browser: Send + Sync {
    fn open(&self, url: &Url) -> Result<()>;
}

/// The desktop's browser, through `xdg-open` (arguments passed directly, no shell).
pub struct XdgOpen;

impl Browser for XdgOpen {
    fn open(&self, url: &Url) -> Result<()> {
        let mut child = std::process::Command::new("xdg-open")
            .arg(url.as_str())
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

/// The one-shot listener the browser comes back to.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

const PAGE_OK: &str = "You are signed in to Grapple with tether. You can close this tab.";
const PAGE_DENIED: &str = "Grapple was not signed in. You can close this tab.";

fn page(status: &str, text: &str) -> String {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Grapple</title>\
         <meta name=\"color-scheme\" content=\"dark light\"></head><body><p>{text}</p></body></html>"
    );
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

enum Callback {
    Code(Secret),
    Denied,
    /// Not the answer we wait for (wrong path, wrong state, a favicon): keep waiting.
    Ignore,
}

impl Loopback {
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Loopback { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the browser to bring back a code for `state` (or the person's "Cancel").
    pub async fn wait(self, state: &Secret, timeout: Duration) -> Result<Secret> {
        let deadline = Instant::now() + timeout;
        loop {
            let accepted = tokio::time::timeout_at(deadline, self.listener.accept()).await;
            let (stream, _) = match accepted {
                Err(_) => return Err(Error::Timeout),
                Ok(r) => r?,
            };
            match tokio::time::timeout(Duration::from_secs(10), handle(stream, state)).await {
                Ok(Ok(Callback::Code(code))) => return Ok(code),
                Ok(Ok(Callback::Denied)) => return Err(Error::Denied),
                // A broken or slow connection is not the end of the sign-in.
                _ => continue,
            }
        }
    }
}

async fn handle(mut stream: TcpStream, state: &Secret) -> Result<Callback> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).await?;
        if n == 0 || buf.len() > 8192 {
            return Ok(Callback::Ignore);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = Secret::new(String::from_utf8_lossy(&buf).into_owned());
    buf.fill(0);
    let line = head.expose().lines().next().unwrap_or("");
    let mut parts = line.split(' ');
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok();
    let Some(url) = url.filter(|u| method == "GET" && u.path() == "/callback") else {
        stream
            .write_all(page("404 Not Found", "Not here.").as_bytes())
            .await?;
        return Ok(Callback::Ignore);
    };
    let get = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| Secret::new(v))
    };
    let state_ok =
        get("state").is_some_and(|s| ct_eq(s.expose().as_bytes(), state.expose().as_bytes()));
    if !state_ok {
        stream
            .write_all(
                page(
                    "400 Bad Request",
                    "This sign-in is not the one Grapple started.",
                )
                .as_bytes(),
            )
            .await?;
        return Ok(Callback::Ignore);
    }
    if get("error").is_some() {
        stream
            .write_all(page("200 OK", PAGE_DENIED).as_bytes())
            .await?;
        return Ok(Callback::Denied);
    }
    match get("code").filter(|c| !c.expose().is_empty() && c.expose().len() <= 256) {
        Some(code) => {
            stream.write_all(page("200 OK", PAGE_OK).as_bytes()).await?;
            Ok(Callback::Code(code))
        }
        None => {
            stream
                .write_all(page("400 Bad Request", "No code came back.").as_bytes())
                .await?;
            Ok(Callback::Ignore)
        }
    }
}

/// `POST /oauth2/token`'s answer.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: Secret,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: Secret,
    #[serde(default)]
    pub scope: String,
}

#[derive(Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: String,
}

async fn token_request(
    http: &reqwest::Client,
    config: &TetherConfig,
    form: &[(&str, &str)],
) -> Result<TokenResponse> {
    let res = http
        .post(config.api("/oauth2/token"))
        .form(form)
        .send()
        .await?;
    let status = res.status();
    if status.is_success() {
        let tokens: TokenResponse = res.json().await?;
        if !tokens.token_type.eq_ignore_ascii_case("bearer") {
            return Err(Error::Protocol("token_type is not Bearer".into()));
        }
        return Ok(tokens);
    }
    if status.as_u16() == 400
        && let Ok(body) = res.json::<OAuthErrorBody>().await
    {
        return Err(Error::OAuth {
            error: body.error,
            description: body.error_description,
        });
    }
    Err(Error::Status(status.as_u16()))
}

/// Trade the code (once, within two minutes) for tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    config: &TetherConfig,
    code: &Secret,
    redirect_uri: &str,
    verifier: &Secret,
) -> Result<TokenResponse> {
    token_request(
        http,
        config,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code.expose()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier.expose()),
        ],
    )
    .await
}

/// Trade the refresh token for a new pair (the old pair stops working).
pub async fn refresh(
    http: &reqwest::Client,
    config: &TetherConfig,
    refresh_token: &Secret,
) -> Result<TokenResponse> {
    token_request(
        http,
        config,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token.expose()),
        ],
    )
    .await
}

/// Sign out at tether (RFC 7009; 200 even for unknown tokens).
pub async fn revoke(http: &reqwest::Client, config: &TetherConfig, token: &Secret) -> Result<()> {
    let res = http
        .post(config.api("/oauth2/token/revoke"))
        .form(&[("token", token.expose()), ("client_id", CLIENT_ID)])
        .send()
        .await?;
    if res.status().is_success() {
        Ok(())
    } else {
        Err(Error::Status(res.status().as_u16()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_rfc_7636_appendix_b() {
        assert_eq!(
            s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_and_state_are_random_and_well_formed() {
        let a = pkce();
        let b = pkce();
        assert_eq!(a.verifier.expose().len(), 43);
        assert_eq!(a.challenge.len(), 43);
        assert_ne!(a.verifier, b.verifier);
        assert_eq!(s256(a.verifier.expose()), a.challenge);
    }

    #[test]
    fn authorize_url_has_every_parameter() {
        let config = TetherConfig::new("https://tether.example").unwrap();
        let url = authorize_url(&config, "http://127.0.0.1:4321/callback", "st", "ch", SCOPES);
        assert_eq!(url.path(), "/oauth2/authorize");
        let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "grapple");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:4321/callback");
        assert_eq!(q["scope"], "identify presence.game friends.read invites");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "st");
        assert_eq!(q["code_challenge"], "ch");
        let older = authorize_url(&config, "http://127.0.0.1:4321/callback", "st", "ch", SCOPES_WITHOUT_INVITES);
        let q: std::collections::HashMap<_, _> = older.query_pairs().into_owned().collect();
        assert_eq!(q["scope"], "identify presence.game friends.read");
    }
}
