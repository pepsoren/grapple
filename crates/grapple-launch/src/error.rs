//! Errors for launching. Messages are shown to the user, so they say what to do and never contain tokens.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("could not find {what}: {detail}")]
    Missing { what: &'static str, detail: String },
    #[error("could not read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("could not parse {path}: {source}")]
    Parse { path: PathBuf, source: serde_json::Error },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not start {program}: {source}")]
    Spawn { program: String, source: std::io::Error },
    /// The Epic sign-in cannot be used any more; the user has to sign in again in Heroic.
    #[error("Epic sign-in expired ({0}); sign in again in Heroic")]
    EpicSignedOut(String),
    /// Epic could not be reached.
    #[error("could not reach Epic: {0}")]
    Network(String),
    #[error("Epic answered with an unexpected response: {0}")]
    EpicProtocol(String),
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0} is not running")]
    NotRunning(String),
    #[error("the game exited right after starting (exit code {code}); see {log}")]
    ExitedEarly { code: String, log: String },
    /// umu could not download the Steam Linux Runtime the game needs (details in the game's log).
    #[error("Runtime download failed. Check your connection.")]
    RuntimeDownload,
    #[error("database error: {0}")]
    Db(String),
}

pub type Result<T> = std::result::Result<T, LaunchError>;

impl From<grapple_core::db::Error> for LaunchError {
    fn from(e: grapple_core::db::Error) -> Self {
        LaunchError::Db(e.to_string())
    }
}

impl From<reqwest::Error> for LaunchError {
    fn from(e: reqwest::Error) -> Self {
        // reqwest's Display can include the URL; ours never carry secrets (they go in headers and bodies),
        // but strip it anyway.
        LaunchError::Network(e.without_url().to_string())
    }
}
