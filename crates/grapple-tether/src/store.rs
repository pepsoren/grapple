//! Where the tokens live: the Secret Service keyring (on Windows the Credential Manager), service
//! [`KEYRING_SERVICE`], one entry per instance, never a plain file. Tests use [`MemoryStore`].
//!
//! The calls are blocking (D-Bus); the crate only makes them from `spawn_blocking`.

use crate::config::TetherConfig;
use crate::error::{Error, Result};
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The keyring service name.
pub const KEYRING_SERVICE: &str = "grapple";

/// What is kept between runs. `Debug` never shows the tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: Secret,
    pub refresh_token: Secret,
    /// When the access token runs out, Unix seconds.
    pub expires_at: i64,
    pub scopes: Vec<String>,
}

pub trait TokenStore: Send + Sync + 'static {
    fn load(&self) -> Result<Option<StoredTokens>>;
    fn save(&self, tokens: &StoredTokens) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

/// The system keyring (GNOME Keyring, KWallet...) over the Secret Service API.
pub struct KeyringStore {
    entry: keyring::Entry,
}

fn keyring_error(err: keyring::Error) -> Error {
    // keyring's messages name the service and user, never the secret.
    Error::Keyring(err.to_string())
}

impl KeyringStore {
    pub fn new(config: &TetherConfig) -> Result<Self> {
        let entry =
            keyring::Entry::new(KEYRING_SERVICE, &config.keyring_user()).map_err(keyring_error)?;
        Ok(KeyringStore { entry })
    }
}

impl TokenStore for KeyringStore {
    fn load(&self) -> Result<Option<StoredTokens>> {
        match self.entry.get_password() {
            Ok(json) => {
                let json = Secret::new(json);
                match serde_json::from_str(json.expose()) {
                    Ok(tokens) => Ok(Some(tokens)),
                    Err(_) => {
                        // Not ours or damaged: forget it rather than fail every start.
                        self.clear()?;
                        Ok(None)
                    }
                }
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(keyring_error(err)),
        }
    }

    fn save(&self, tokens: &StoredTokens) -> Result<()> {
        let json =
            Secret::new(serde_json::to_string(tokens).map_err(|e| Error::Keyring(e.to_string()))?);
        self.entry
            .set_password(json.expose())
            .map_err(keyring_error)
    }

    fn clear(&self) -> Result<()> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(keyring_error(err)),
        }
    }
}

/// Keeps the tokens in memory only (tests, or a session that must not persist).
#[derive(Default)]
pub struct MemoryStore {
    tokens: Mutex<Option<StoredTokens>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A copy of what is stored (tests).
    pub fn peek(&self) -> Option<StoredTokens> {
        self.tokens.lock().unwrap().clone()
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<StoredTokens>> {
        Ok(self.tokens.lock().unwrap().clone())
    }
    fn save(&self, tokens: &StoredTokens) -> Result<()> {
        *self.tokens.lock().unwrap() = Some(tokens.clone());
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        *self.tokens.lock().unwrap() = None;
        Ok(())
    }
}
