//! A string that must never be shown: tokens, codes, PKCE verifiers, `state`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Holds a secret. `Debug` prints `Secret(***)`, there is no `Display`, and the bytes are
/// overwritten (best effort) when it is dropped.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The secret itself, for putting on the wire. Never log what this returns.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // SAFETY: only zero bytes are written, which keeps the String valid UTF-8.
        let bytes = unsafe { self.0.as_mut_vec() };
        for b in bytes.iter_mut() {
            // Volatile so the compiler does not drop the writes as dead stores.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// Constant-time comparison (for `state`), so timing does not reveal how much matched.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let s = Secret::new("tapp.supersecret");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(s.expose(), "tapp.supersecret");
    }

    #[test]
    fn ct_eq_compares() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }
}
