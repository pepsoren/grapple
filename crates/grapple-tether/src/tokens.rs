//! The current tokens and their refresh (contract section 2, "Token lifetime and refresh").
//!
//! One lock around everything: two refreshes with the same refresh token would make tether see a
//! reused token and revoke the sign-in, so they must never race. A new pair is saved to the store
//! before it is used.

use crate::config::TetherConfig;
use crate::error::{Error, Result};
use crate::oauth::{self, TokenResponse};
use crate::secret::Secret;
use crate::store::{StoredTokens, TokenStore};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Refresh when the access token has less than this left (seconds).
pub const REFRESH_MARGIN_SECS: i64 = 24 * 3600;

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct TokenManager {
    config: TetherConfig,
    http: reqwest::Client,
    store: Arc<dyn TokenStore>,
    /// `None` until loaded; `Some(None)` = loaded, nothing stored.
    cache: Mutex<Option<Option<StoredTokens>>>,
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Keyring(e.to_string()))?
}

impl TokenManager {
    pub fn new(config: TetherConfig, http: reqwest::Client, store: Arc<dyn TokenStore>) -> Self {
        TokenManager {
            config,
            http,
            store,
            cache: Mutex::new(None),
        }
    }

    async fn loaded<'a>(
        &self,
        guard: &'a mut tokio::sync::MutexGuard<'_, Option<Option<StoredTokens>>>,
    ) -> Result<&'a mut Option<StoredTokens>> {
        if guard.is_none() {
            let store = self.store.clone();
            let tokens = blocking(move || store.load()).await?;
            **guard = Some(tokens);
        }
        Ok(guard.as_mut().expect("just loaded"))
    }

    pub async fn is_signed_in(&self) -> Result<bool> {
        let mut guard = self.cache.lock().await;
        Ok(self.loaded(&mut guard).await?.is_some())
    }

    /// What the stored sign-in was granted (empty when signed out).
    pub async fn scopes(&self) -> Result<Vec<String>> {
        let mut guard = self.cache.lock().await;
        Ok(self.loaded(&mut guard).await?.as_ref().map(|t| t.scopes.clone()).unwrap_or_default())
    }

    /// Keep a fresh pair from the token endpoint; returns the pair it replaced, if any.
    pub async fn set(&self, response: TokenResponse) -> Result<Option<StoredTokens>> {
        let mut guard = self.cache.lock().await;
        let previous = self.loaded(&mut guard).await?.clone();
        self.save_locked(&mut guard, response).await?;
        Ok(previous)
    }

    async fn save_locked(
        &self,
        guard: &mut tokio::sync::MutexGuard<'_, Option<Option<StoredTokens>>>,
        response: TokenResponse,
    ) -> Result<()> {
        let tokens = StoredTokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: now_secs() + response.expires_in.max(0),
            scopes: response
                .scope
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
        };
        let store = self.store.clone();
        let copy = tokens.clone();
        blocking(move || store.save(&copy)).await?;
        **guard = Some(Some(tokens));
        Ok(())
    }

    async fn forget_locked(
        &self,
        guard: &mut tokio::sync::MutexGuard<'_, Option<Option<StoredTokens>>>,
    ) -> Result<()> {
        **guard = Some(None);
        let store = self.store.clone();
        blocking(move || store.clear()).await
    }

    async fn refresh_locked(
        &self,
        guard: &mut tokio::sync::MutexGuard<'_, Option<Option<StoredTokens>>>,
    ) -> Result<Secret> {
        let current = self
            .loaded(guard)
            .await?
            .clone()
            .ok_or(Error::NotSignedIn)?;
        match oauth::refresh(&self.http, &self.config, &current.refresh_token).await {
            Ok(response) => {
                let access = response.access_token.clone();
                self.save_locked(guard, response).await?;
                Ok(access)
            }
            Err(Error::OAuth { error, .. }) if error == "invalid_grant" => {
                self.forget_locked(guard).await?;
                Err(Error::SignedOut)
            }
            Err(err) => Err(err),
        }
    }

    /// A usable access token, refreshed first if it is close to running out. A failed refresh
    /// (network) still returns the old token while it has not run out.
    pub async fn access_token(&self) -> Result<Secret> {
        let mut guard = self.cache.lock().await;
        let current = self
            .loaded(&mut guard)
            .await?
            .clone()
            .ok_or(Error::NotSignedIn)?;
        if current.expires_at - now_secs() > REFRESH_MARGIN_SECS {
            return Ok(current.access_token);
        }
        match self.refresh_locked(&mut guard).await {
            Ok(token) => Ok(token),
            Err(Error::SignedOut) => Err(Error::SignedOut),
            Err(_) if current.expires_at > now_secs() => Ok(current.access_token),
            Err(err) => Err(err),
        }
    }

    /// Refresh now, because tether refused the access token (401, gateway 4004). `rejected` is the
    /// token that was refused: if another task refreshed meanwhile, its newer token is returned
    /// instead of refreshing again.
    pub async fn refresh_after_rejection(&self, rejected: &Secret) -> Result<Secret> {
        let mut guard = self.cache.lock().await;
        let current = self
            .loaded(&mut guard)
            .await?
            .clone()
            .ok_or(Error::SignedOut)?;
        if &current.access_token != rejected {
            return Ok(current.access_token);
        }
        self.refresh_locked(&mut guard).await
    }

    /// Forget the tokens here (after a revoke, or when tether says the sign-in is gone).
    pub async fn forget(&self) -> Result<()> {
        let mut guard = self.cache.lock().await;
        self.forget_locked(&mut guard).await
    }

    /// Sign out at tether, then forget. Returns whether tether confirmed it; the local tokens are
    /// gone either way.
    pub async fn sign_out(&self) -> Result<bool> {
        let mut guard = self.cache.lock().await;
        let current = self.loaded(&mut guard).await?.clone();
        let revoked = match &current {
            Some(t) => oauth::revoke(&self.http, &self.config, &t.refresh_token)
                .await
                .is_ok(),
            None => true,
        };
        self.forget_locked(&mut guard).await?;
        Ok(revoked)
    }
}
