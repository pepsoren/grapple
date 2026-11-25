// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from legendary (GPLv3); see NOTICE.md.
//! Epic sign-in, ported from legendary (https://github.com/legendary-gl/legendary, master at the time of writing).
//!
//! What it does for a launch:
//! 1. read `user.json` (legendary's login file, shared with Heroic) under legendary's own lock file,
//! 2. refresh the access token when it expires within 10 minutes and **write the rotated tokens back**
//!    atomically (Epic invalidates the old refresh token, so Heroic/legendary would be logged out otherwise),
//! 3. fetch a one-time exchange code, which is what the game takes as `-AUTH_PASSWORD`.
//!
//! Secrets are [`Secret`]s: their `Debug`/`Display` are redacted, and nothing in this file logs a token.

use crate::error::{LaunchError, Result};
use crate::plan::Secret;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// legendary/api/egs.py:21 `_user_agent`
pub const USER_AGENT: &str = "UELauncher/11.0.1-14907503+++Portal+Release-Live Windows/10.0.19041.1.256.64bit";
/// legendary/api/egs.py:24 `_user_basic` (the public Epic Games Launcher client id; legendary ships it in source)
pub const CLIENT_ID: &str = "34a02cf8f4414e29b15921876da36f9a";
/// legendary/api/egs.py:25 `_pw_basic` (public too, same file)
pub const CLIENT_SECRET: &str = "daafbccc737745039dffe53d94fc76cf";
/// legendary/api/egs.py:28 `_oauth_host`
pub const OAUTH_BASE: &str = "https://account-public-service-prod03.ol.epicgames.com";
/// legendary/api/egs.py:32 `_ecommerce_host`
pub const ECOMMERCE_BASE: &str = "https://ecommerceintegration-public-service-ecomprod02.ol.epicgames.com";
/// legendary/core.py:225,252 `abs(td.total_seconds()) > 600`: refresh when less than 10 minutes remain.
pub const REFRESH_MARGIN_SECS: i64 = 600;

#[derive(Debug, Clone)]
pub struct EpicApi {
    pub oauth_base: String,
    pub ecommerce_base: String,
    pub timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for EpicApi {
    fn default() -> Self {
        EpicApi {
            oauth_base: OAUTH_BASE.into(),
            ecommerce_base: ECOMMERCE_BASE.into(),
            // legendary uses 10 s (egs.py:39 `timeout=10.0`)
            timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(4),
        }
    }
}

/// The parts of `user.json` a launch needs. The rest stays in `raw` and is written back untouched.
#[derive(Clone)]
pub struct UserData {
    raw: Map<String, Value>,
}

impl fmt::Debug for UserData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserData")
            .field("account_id", &self.str_field("account_id"))
            .field("tokens", &"<redacted>")
            .finish()
    }
}

impl UserData {
    pub fn parse(bytes: &[u8], path: &Path) -> Result<Self> {
        let v: Value = serde_json::from_slice(bytes).map_err(|source| LaunchError::Parse { path: path.to_path_buf(), source })?;
        match v {
            Value::Object(raw) => Ok(UserData { raw }),
            _ => Err(LaunchError::Config(format!("{} is not a JSON object", path.display()))),
        }
    }

    fn str_field(&self, k: &str) -> Option<String> {
        self.raw.get(k).and_then(Value::as_str).map(str::to_string)
    }

    pub fn account_id(&self) -> Result<String> {
        self.str_field("account_id").ok_or_else(|| LaunchError::EpicSignedOut("no account id".into()))
    }
    pub fn display_name(&self) -> Result<String> {
        self.str_field("displayName").ok_or_else(|| LaunchError::EpicSignedOut("no display name".into()))
    }
    pub fn identity(&self) -> Result<Identity> {
        Ok(Identity { account_id: self.account_id()?, display_name: self.display_name()? })
    }
    fn access_token(&self) -> Option<Secret> {
        self.str_field("access_token").map(Secret::new)
    }
    fn refresh_token(&self) -> Option<Secret> {
        self.str_field("refresh_token").map(Secret::new)
    }

    /// legendary core.py:222-228: `expires_at` is ISO-8601 UTC with a trailing `Z`.
    pub fn access_valid(&self, now: DateTime<Utc>) -> bool {
        let Some(exp) = self.str_field("expires_at").and_then(|s| DateTime::parse_from_rfc3339(&s).ok()) else { return false };
        self.access_token().is_some() && (exp.with_timezone(&Utc) - now).num_seconds() > REFRESH_MARGIN_SECS
    }

    /// Overlay a token response. legendary replaces the whole dict (core.py:278 `lock.data = userdata`); overlaying
    /// keeps every field the response does not mention, which can only be a superset.
    fn merge(&mut self, response: Map<String, Value>) {
        for (k, v) in response {
            self.raw.insert(k, v);
        }
    }

    fn to_json(&self) -> Vec<u8> {
        // 2-space indent and sorted keys, like legendary's `json.dump(..., indent=2, sort_keys=True)`
        // (lfs/utils.py LockedJSONData.__exit__). `Map` is ordered by key unless serde_json's preserve_order is on.
        let mut out = serde_json::to_vec_pretty(&Value::Object(self.raw.clone())).unwrap_or_default();
        out.push(b'\n');
        out
    }
}

/// Who is signed in. Not secret (but only read from `user.json` at launch time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub account_id: String,
    pub display_name: String,
}

/// An exclusive lock on `<user.json>.lock`: the same lock legendary takes, so Grapple, Heroic and legendary never
/// refresh at the same time. Python's `filelock` uses `flock` on Linux and `msvcrt.locking(fd, LK_NBLCK, 1)` on
/// Windows, a lock on the first byte of the file, which is what `LockFileEx` takes here. Released when dropped.
pub struct FileLock {
    _file: std::fs::File,
}

impl FileLock {
    pub fn acquire(lock_path: &Path, timeout: Duration) -> Result<FileLock> {
        let file = private(OpenOptions::new().create(true).truncate(false).write(true)).open(lock_path)?;
        let deadline = Instant::now() + timeout;
        loop {
            match try_lock(&file) {
                Ok(true) => return Ok(FileLock { _file: file }),
                Ok(false) => {}
                Err(err) => return Err(err.into()),
            }
            if Instant::now() > deadline {
                return Err(LaunchError::Timeout("another program (Heroic?) is holding the Epic login file".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Owner-only permissions for files that hold tokens (Unix: 0600; Windows: the user's profile folder already is).
fn private(o: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    o.mode(0o600);
    o
}

/// `Ok(false)`: someone else holds the lock.
#[cfg(unix)]
fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: flock(2) on a file descriptor we own.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) { Ok(false) } else { Err(err) }
}

/// `Ok(false)`: someone else holds the lock. The lock goes away when the file is closed.
#[cfg(windows)]
fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx};
    use windows_sys::Win32::System::IO::OVERLAPPED;
    // SAFETY: OVERLAPPED is plain data; all zero = offset 0, no event.
    let mut at: OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: a synchronous LockFileEx on a handle we own, byte range [0, 1).
    let ok = unsafe { LockFileEx(file.as_raw_handle(), LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, 1, 0, &mut at) };
    if ok != 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) { Ok(false) } else { Err(err) }
}

/// Write `bytes` to `path` atomically: temp file in the same directory, mode 0600, fsync, rename, fsync dir.
/// On Windows the rename replaces the file in one step too (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, what
/// `std::fs::rename` does there); a reader that has the file open without delete sharing makes it fail for a
/// moment, so it is retried briefly.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(|| LaunchError::Config("login file has no directory".into()))?;
    let tmp = dir.join(format!(
        ".{}.grapple-{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("user.json"),
        std::process::id()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut f = private(OpenOptions::new().create(true).truncate(true).write(true)).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        rename_retrying(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(result?)
}

fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied && tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            r => return r,
        }
    }
}

/// legendary's `user.json` where Heroic keeps it (`LaunchSettings::default().legendary_dir`).
pub fn default_user_json() -> PathBuf {
    crate::settings::LaunchSettings::default().legendary_dir.join("user.json")
}

/// What a launch needs from Epic's side.
pub struct GameToken {
    pub identity: Identity,
    /// The one-time exchange code (`-AUTH_PASSWORD`).
    pub code: Secret,
    /// Needed for the ownership token of games with `requires_ot`.
    pub access: Secret,
}

impl fmt::Debug for GameToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GameToken").field("identity", &self.identity).field("code", &"<redacted>").finish()
    }
}

pub struct EpicAuth {
    api: EpicApi,
    user_json: PathBuf,
    client: reqwest::Client,
}

impl EpicAuth {
    pub fn new(api: EpicApi, user_json: PathBuf) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(api.timeout)
            .connect_timeout(api.connect_timeout)
            .build()?;
        Ok(EpicAuth { api, user_json, client })
    }

    fn lock_path(&self) -> PathBuf {
        let mut p = self.user_json.clone().into_os_string();
        p.push(".lock");
        PathBuf::from(p)
    }

    fn read_user(&self) -> Result<UserData> {
        let bytes = std::fs::read(&self.user_json).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => LaunchError::EpicSignedOut("no login file; sign in to Epic in Heroic first".into()),
            _ => LaunchError::Read { path: self.user_json.clone(), source },
        })?;
        UserData::parse(&bytes, &self.user_json)
    }

    /// Make sure `user.json` holds an access token with at least 10 minutes left, refreshing (and writing back)
    /// if needed. `force` refreshes even when it looks valid (after the server rejected the token).
    pub async fn ensure_session(&self, force: bool) -> Result<UserData> {
        // Fast path without the lock: a valid token needs no write. Heroic/legendary rewrite the file in place
        // (under the lock), so a read that catches it half-written fails to parse: then wait for the lock below.
        match self.read_user() {
            Ok(user) if !force && user.access_valid(Utc::now()) => return Ok(user),
            Ok(_) | Err(LaunchError::Parse { .. }) => {}
            Err(e) => return Err(e),
        }
        let lock_path = self.lock_path();
        let lock = tokio::task::spawn_blocking(move || FileLock::acquire(&lock_path, Duration::from_secs(15)))
            .await
            .map_err(|e| LaunchError::Config(e.to_string()))??;
        // Re-read under the lock: Heroic may have refreshed while we waited (legendary does the same, core.py:211+).
        let mut user = self.read_user()?;
        if !force && user.access_valid(Utc::now()) {
            drop(lock);
            return Ok(user);
        }
        let refresh = user.refresh_token().ok_or_else(|| LaunchError::EpicSignedOut("no refresh token".into()))?;
        let response = self.refresh(&refresh).await?;
        user.merge(response);
        write_atomic(&self.user_json, &user.to_json())?;
        drop(lock);
        Ok(user)
    }

    /// legendary egs.py:100-144 `start_session(refresh_token=...)`.
    async fn refresh(&self, refresh_token: &Secret) -> Result<Map<String, Value>> {
        let resp = self
            .client
            .post(format!("{}/account/api/oauth/token", self.api.oauth_base))
            .basic_auth(CLIENT_ID, Some(CLIENT_SECRET))
            .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh_token.expose()), ("token_type", "eg1")])
            .send()
            .await?;
        let status = resp.status();
        // egs.py:124 only raises on >= 500 (server trouble: treat as "Epic unreachable").
        if status.is_server_error() {
            return Err(LaunchError::Network(format!("Epic answered {status}")));
        }
        let body: Value = resp.json().await.map_err(|e| LaunchError::EpicProtocol(e.without_url().to_string()))?;
        if let Some(code) = body.get("errorCode").and_then(Value::as_str) {
            // egs.py:128-134 InvalidCredentialsError
            return Err(LaunchError::EpicSignedOut(code.to_string()));
        }
        if status.is_client_error() {
            return Err(LaunchError::EpicSignedOut(format!("HTTP {status}")));
        }
        match body {
            Value::Object(m) if m.contains_key("access_token") && m.contains_key("refresh_token") => Ok(m),
            _ => Err(LaunchError::EpicProtocol("token response has no tokens".into())),
        }
    }

    /// legendary egs.py:150-154 `get_game_token`: `GET /account/api/oauth/exchange` with the bearer token.
    async fn exchange(&self, access: &Secret) -> Result<ExchangeResult> {
        let resp = self
            .client
            .get(format!("{}/account/api/oauth/exchange", self.api.oauth_base))
            .bearer_auth(access.expose())
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Ok(ExchangeResult::Rejected);
        }
        if !status.is_success() {
            return Err(LaunchError::Network(format!("Epic answered {status}")));
        }
        let body: Value = resp.json().await.map_err(|e| LaunchError::EpicProtocol(e.without_url().to_string()))?;
        let code = body
            .get("code")
            .and_then(Value::as_str)
            .ok_or_else(|| LaunchError::EpicProtocol("exchange response has no code".into()))?;
        Ok(ExchangeResult::Code(Secret::new(code)))
    }

    /// The whole sign-in step of a launch: valid token, exchange code, identity.
    ///
    /// Unlike legendary we skip the extra `/oauth/verify` round trip (egs.py:83 `resume_session`): if the token is
    /// bad the exchange call says so, and we then refresh once and retry.
    pub async fn game_token(&self) -> Result<GameToken> {
        let mut forced = false;
        loop {
            let user = self.ensure_session(forced).await?;
            let access = user.access_token().ok_or_else(|| LaunchError::EpicSignedOut("no access token".into()))?;
            match self.exchange(&access).await? {
                ExchangeResult::Code(code) => return Ok(GameToken { identity: user.identity()?, code, access }),
                ExchangeResult::Rejected if !forced => forced = true,
                ExchangeResult::Rejected => return Err(LaunchError::EpicSignedOut("Epic rejected the new token".into())),
            }
        }
    }

    /// A fresh one-time exchange code for a web sign-in (grapple's store window signs in to the Epic store with it).
    /// Same path as a launch: refresh under the lock and write the rotated tokens back when needed, retry once if
    /// Epic rejects the token. The code is single use and short-lived; use it for exactly one navigation.
    pub async fn exchange_code(&self) -> Result<Secret> {
        Ok(self.game_token().await?.code)
    }

    /// A valid access token (refreshed and written back under the lock when needed) and whose it is. For read-only
    /// Epic services called with the launcher's token (e.g. catalog prices).
    pub async fn session(&self) -> Result<(Identity, Secret)> {
        let user = self.ensure_session(false).await?;
        let access = user.access_token().ok_or_else(|| LaunchError::EpicSignedOut("no access token".into()))?;
        Ok((user.identity()?, access))
    }

    /// Who is signed in (no network). Used by dry runs of real configs only; the `plan` example does not call it.
    pub fn identity(&self) -> Result<Identity> {
        self.read_user()?.identity()
    }

    /// legendary egs.py:156-163 `get_ownership_token`: `POST .../ownershipToken` with `nsCatalogItemId=<ns>:<id>`.
    pub async fn ownership_token(&self, identity: &Identity, access: &Secret, namespace: &str, catalog_item_id: &str) -> Result<Vec<u8>> {
        let url = format!(
            "{}/ecommerceintegration/api/public/platforms/EPIC/identities/{}/ownershipToken",
            self.api.ecommerce_base, identity.account_id
        );
        let resp = self
            .client
            .post(url)
            .bearer_auth(access.expose())
            .form(&[("nsCatalogItemId", format!("{namespace}:{catalog_item_id}"))])
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(LaunchError::Network(format!("Epic answered {} for the ownership token", resp.status())));
        }
        Ok(resp.bytes().await?.to_vec())
    }
}

enum ExchangeResult {
    Code(Secret),
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validity_window() {
        let now = DateTime::parse_from_rfc3339("2026-10-18T00:00:00Z").unwrap().with_timezone(&Utc);
        let mk = |exp: &str| UserData::parse(format!(r#"{{"access_token":"a","expires_at":"{exp}"}}"#).as_bytes(), Path::new("x")).unwrap();
        assert!(mk("2026-10-18T00:10:01.000Z").access_valid(now));
        assert!(!mk("2026-10-18T00:09:59.000Z").access_valid(now));
        assert!(!mk("2026-10-17T23:00:00.000Z").access_valid(now));
    }

    #[test]
    fn the_login_lock_is_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("user.json.lock");
        let held = FileLock::acquire(&p, Duration::from_millis(100)).unwrap();
        assert!(matches!(FileLock::acquire(&p, Duration::from_millis(150)), Err(LaunchError::Timeout(_))));
        drop(held);
        assert!(FileLock::acquire(&p, Duration::from_millis(100)).is_ok());
    }

    #[test]
    fn debug_is_redacted() {
        let u = UserData::parse(br#"{"access_token":"SECRETA","refresh_token":"SECRETR","account_id":"abc"}"#, Path::new("x")).unwrap();
        let text = format!("{u:?}");
        assert!(!text.contains("SECRET"));
    }
}
