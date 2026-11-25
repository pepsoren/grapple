//! [`Tether`]: one tether instance and its sign-in.

use crate::api::Api;
use crate::config::TetherConfig;
use crate::error::{Error, Result};
use crate::gateway::{self, GatewayHandle, GatewayOptions};
use crate::model::TetherUser;
use crate::oauth::{self, Browser, Loopback, SIGN_IN_TIMEOUT};
use crate::store::{KeyringStore, TokenStore};
use crate::tokens::TokenManager;
use std::sync::Arc;
use std::time::Duration;

pub struct Tether {
    config: TetherConfig,
    http: reqwest::Client,
    tokens: Arc<TokenManager>,
    api: Arc<Api>,
    sign_in_timeout: Duration,
}

impl Tether {
    /// With any token store ([`crate::MemoryStore`] in tests).
    pub fn new(config: TetherConfig, store: Arc<dyn TokenStore>) -> Result<Self> {
        // Fails harmlessly when a provider is already installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let http = reqwest::Client::builder()
            .user_agent(concat!("grapple/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            // Never follow a redirect with a token on it.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let tokens = Arc::new(TokenManager::new(config.clone(), http.clone(), store));
        let api = Arc::new(Api::new(config.clone(), http.clone(), tokens.clone()));
        Ok(Tether {
            config,
            http,
            tokens,
            api,
            sign_in_timeout: SIGN_IN_TIMEOUT,
        })
    }

    /// The app's way: tokens in the system keyring (Secret Service), service "grapple".
    pub fn with_keyring(config: TetherConfig) -> Result<Self> {
        let store = KeyringStore::new(&config)?;
        Tether::new(config, Arc::new(store))
    }

    /// How long [`Tether::sign_in`] waits for the browser (default 5 minutes).
    pub fn set_sign_in_timeout(&mut self, timeout: Duration) {
        self.sign_in_timeout = timeout;
    }

    pub fn config(&self) -> &TetherConfig {
        &self.config
    }

    pub fn api(&self) -> &Api {
        &self.api
    }

    pub async fn is_signed_in(&self) -> Result<bool> {
        self.tokens.is_signed_in().await
    }

    /// What the stored sign-in was granted (`identify`, `invites`, ...); empty when signed out.
    pub async fn scopes(&self) -> Result<Vec<String>> {
        self.tokens.scopes().await
    }

    /// "Sign in with tether": opens the browser at tether's authorize page and waits for the
    /// answer on `http://127.0.0.1:<free port>/callback`. On success the tokens are in the store.
    /// First checks that this tether can sign apps in at all, so the browser never opens onto a
    /// "not found" page: [`Error::AppsUnsupported`] or [`Error::Unreachable`] instead.
    /// Asks for `invites` only when this tether has game invites (an older one refuses the scope).
    /// Signing in again is also how a sign-in without `invites` allows them ("Allow invites").
    pub async fn sign_in(&self, browser: &dyn Browser) -> Result<TetherUser> {
        match crate::probe::probe(&self.http, &self.config).await {
            crate::probe::Probe::Ok => {}
            crate::probe::Probe::Unsupported => return Err(Error::AppsUnsupported),
            crate::probe::Probe::Unreachable => {
                let host = self.config.origin().split_once("://").map_or(self.config.origin(), |(_, h)| h);
                return Err(Error::Unreachable(host.to_string()));
            }
        }
        let scopes = if self.api.invites_supported().await { oauth::SCOPES } else { oauth::SCOPES_WITHOUT_INVITES };
        let loopback = Loopback::bind().await?;
        let redirect_uri = loopback.redirect_uri();
        let state = oauth::random_b64(32);
        let pkce = oauth::pkce();
        let url = oauth::authorize_url(
            &self.config,
            &redirect_uri,
            state.expose(),
            &pkce.challenge,
            scopes,
        );
        browser.open(&url)?;
        drop(url);
        let code = loopback.wait(&state, self.sign_in_timeout).await?;
        let tokens = oauth::exchange_code(
            &self.http,
            &self.config,
            &code,
            &redirect_uri,
            &pkce.verifier,
        )
        .await?;
        // Signing in again replaces the old sign-in: tell tether to drop it (best effort).
        if let Some(old) = self.tokens.set(tokens).await? {
            let _ = oauth::revoke(&self.http, &self.config, &old.refresh_token).await;
        }
        self.api.me().await
    }

    /// Start the gateway task (needs a sign-in; without one it reports `SignedOut` and stops).
    pub fn connect(&self) -> GatewayHandle {
        self.connect_with(GatewayOptions::default())
    }

    pub fn connect_with(&self, options: GatewayOptions) -> GatewayHandle {
        gateway::spawn(self.api.clone(), self.tokens.clone(), options)
    }

    /// Sign out: close the gateway (if given), revoke at tether, delete the tokens from the store.
    /// The tokens are deleted even when tether cannot be reached; then `Err` says the revoke did
    /// not happen (the sign-in stays listed in tether's Settings until it is signed out there).
    pub async fn sign_out(&self, gateway: Option<GatewayHandle>) -> Result<()> {
        if let Some(g) = gateway {
            g.shutdown().await;
        }
        if self.tokens.sign_out().await? {
            Ok(())
        } else {
            Err(Error::Gateway(
                "signed out here, but tether could not be told".into(),
            ))
        }
    }
}
