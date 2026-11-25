use std::path::PathBuf;

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// What went wrong. [`Error::short`] is the text the UI shows on the item (terse, no explanation).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A manifest or chunk file is not what the format says.
    #[error("bad data: {0}")]
    Format(String),
    #[error("{path}: {source}")]
    File { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The network failed or a server answered with an error.
    #[error("network: {0}")]
    Network(String),
    #[error("Not enough space on {0}")]
    NoSpace(String),
    /// A file still had the wrong hash after its chunks were downloaded again.
    #[error("Download damaged, try again")]
    Damaged,
    #[error("Epic sign-in expired, sign in again in Heroic")]
    SignedOut,
    /// `useSignedUrl` builds (not supported in v1, see docs/DOWNLOADS_PLAN.md "Open").
    #[error("This game needs Heroic to install for now")]
    SignedUrl,
    /// A JSON manifest (an older form; not supported in v1).
    #[error("Old manifest format, install with Heroic")]
    OldManifest,
    #[error("Steam isn't responding")]
    Steam(String),
    /// Something the caller asked for makes no sense (unknown game, bad id, folder in use).
    #[error("{0}")]
    Invalid(String),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
}

impl Error {
    pub fn file(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
        Error::File { path: path.into(), source }
    }

    /// The text shown on a download item.
    pub fn short(&self) -> String {
        match self {
            Error::NoSpace(_) => "Not enough space".into(),
            Error::Network(_) => "Can't reach the download server, try again".into(),
            Error::Format(_) => "Download damaged, try again".into(),
            Error::File { source, .. } | Error::Io(source) if source.kind() == std::io::ErrorKind::StorageFull => "Not enough space".into(),
            Error::File { .. } | Error::Io(_) => "Can't write the game's files".into(),
            Error::Steam(_) => "Steam isn't responding".into(),
            Error::Db(_) => "Something went wrong, try again".into(),
            other => other.to_string(),
        }
    }
}

impl From<grapple_launch::LaunchError> for Error {
    fn from(e: grapple_launch::LaunchError) -> Error {
        use grapple_launch::LaunchError as L;
        match e {
            L::EpicSignedOut(_) => Error::SignedOut,
            L::Network(m) | L::EpicProtocol(m) => Error::Network(m),
            other => Error::Invalid(other.to_string()),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Error {
        // Never keep the URL: chunk URLs are harmless, but signed manifest URLs carry query tokens.
        Error::Network(e.without_url().to_string())
    }
}
