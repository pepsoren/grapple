use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The request failed (network, TLS, timeout). Never contains a URL or a key.
    Http(String),
    /// The service is refusing for now (429); the limiter is backing off.
    RateLimited,
    /// A status the caller did not expect.
    Status(u16),
    /// The answer did not look like what that endpoint returns.
    Parse(String),
    Db(String),
    Io(String),
    /// Something the caller sent is not valid (a key, a currency...).
    Invalid(String),
    NotFound,
    /// The key store (keyring) failed.
    Keyring(String),
    /// A feature that needs setup (ITAD key) or a source that does not exist.
    Unavailable(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(m) => write!(f, "network error: {m}"),
            Error::RateLimited => write!(f, "the store is busy, try again in a bit"),
            Error::Status(s) => write!(f, "unexpected answer ({s})"),
            Error::Parse(m) => write!(f, "unreadable answer: {m}"),
            Error::Db(m) => write!(f, "store database: {m}"),
            Error::Io(m) => write!(f, "io: {m}"),
            Error::Invalid(m) => write!(f, "{m}"),
            Error::NotFound => write!(f, "not found"),
            Error::Keyring(m) => write!(f, "keyring: {m}"),
            Error::Unavailable(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Parse(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}
