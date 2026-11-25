//! Where the IsThereAnyDeal key lives: the system keyring (Secret Service; Windows: the Credential Manager;
//! service "grapple"), never a file and
//! never a log line. Tests and `GRAPPLE_NO_KEYRING=1` use [`MemoryKeys`].
//!
//! The calls are blocking (D-Bus); callers use `spawn_blocking`.

use crate::error::{Error, Result};
use std::fmt;
use std::sync::Mutex;

pub const KEYRING_SERVICE: &str = "grapple";
pub const ITAD_USER: &str = "store.itad-api-key";

/// A key that must not be printed.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(s: impl Into<String>) -> ApiKey {
        ApiKey(s.into())
    }

    /// For the request header. Never log what this returns.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

pub trait KeyStore: Send + Sync + 'static {
    fn load(&self) -> Result<Option<ApiKey>>;
    fn save(&self, key: &ApiKey) -> Result<()>;
    fn clear(&self) -> Result<()>;
}

pub struct KeyringKeys {
    entry: keyring::Entry,
}

fn kerr(e: keyring::Error) -> Error {
    // keyring's messages name the service and user, never the secret.
    Error::Keyring(e.to_string())
}

impl KeyringKeys {
    pub fn new() -> Result<KeyringKeys> {
        Ok(KeyringKeys { entry: keyring::Entry::new(KEYRING_SERVICE, ITAD_USER).map_err(kerr)? })
    }
}

impl KeyStore for KeyringKeys {
    fn load(&self) -> Result<Option<ApiKey>> {
        match self.entry.get_password() {
            Ok(k) if !k.trim().is_empty() => Ok(Some(ApiKey::new(k.trim()))),
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(kerr(e)),
        }
    }

    fn save(&self, key: &ApiKey) -> Result<()> {
        self.entry.set_password(key.expose()).map_err(kerr)
    }

    fn clear(&self) -> Result<()> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(kerr(e)),
        }
    }
}

#[derive(Default)]
pub struct MemoryKeys {
    key: Mutex<Option<ApiKey>>,
}

impl MemoryKeys {
    pub fn new() -> MemoryKeys {
        MemoryKeys::default()
    }
}

impl KeyStore for MemoryKeys {
    fn load(&self) -> Result<Option<ApiKey>> {
        Ok(self.key.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
    fn save(&self, key: &ApiKey) -> Result<()> {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.clone());
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        *self.key.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_key() {
        let k = ApiKey::new("sekrit");
        assert_eq!(format!("{k:?}"), "ApiKey(***)");
        assert_eq!(k.expose(), "sekrit");
    }

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryKeys::new();
        assert_eq!(s.load().unwrap(), None);
        s.save(&ApiKey::new("a")).unwrap();
        assert_eq!(s.load().unwrap().unwrap().expose(), "a");
        s.clear().unwrap();
        assert_eq!(s.load().unwrap(), None);
    }
}
