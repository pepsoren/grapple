//! Errors. None of them ever carries a token, code, verifier or `state`.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a usable tether address: {0}")]
    InvalidInstance(String),
    #[error("sign-in was cancelled in the browser")]
    Denied,
    #[error("sign-in timed out (nobody answered in the browser)")]
    Timeout,
    #[error("not signed in to tether")]
    NotSignedIn,
    /// The sign-in was revoked (from tether's Settings, or a refresh token was refused) or lapsed.
    /// The stored tokens are already gone; show "Sign in with tether" again.
    #[error("the tether sign-in ended; sign in again")]
    SignedOut,
    /// RFC 6749 error from the token endpoint (`invalid_grant`, ...).
    #[error("tether refused the sign-in ({error})")]
    OAuth { error: String, description: String },
    #[error("tether answered HTTP {0}")]
    Status(u16),
    #[error("network error talking to tether: {0}")]
    Http(#[from] reqwest::Error),
    #[error("tether gateway: {0}")]
    Gateway(String),
    #[error("keyring: {0}")]
    Keyring(String),
    #[error("could not open the browser: {0}")]
    Browser(String),
    #[error("avatar: {0}")]
    Avatar(String),
    #[error("game icon: {0}")]
    Icon(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("unexpected answer from tether: {0}")]
    Protocol(String),
    /// Found before the browser opens (see [`crate::probe`]): an older tether without app sign-in.
    #[error("This tether doesn't support signing in from apps yet. Update tether to sign in.")]
    AppsUnsupported,
    /// Found before the browser opens: no answer from the instance (the host, without scheme).
    #[error("Can't reach tether at {0}. Is it running?")]
    Unreachable(String),

    // ---- game invites (contract section 7)
    /// 403 `50001` to a sign-in without the `invites` scope: sign in again to allow invites (re-consent).
    #[error("Allow invites in tether first")]
    NeedsConsent,
    /// 403 `50001` to a sign-in that has `invites`: the wrong side of the invite (the sender accepting,
    /// the recipient cancelling).
    #[error("Only the other person can do that")]
    NotAllowed,
    /// 403 `50007`: not friends, a block, their settings... tether never says which.
    #[error("Can't invite them")]
    CannotInvite,
    /// 404 `10013`: no such person (or yourself).
    #[error("No such person on tether")]
    UnknownUser,
    /// 404 `90270`: not an invite of this account (or the message was deleted).
    #[error("This invite isn't for this account")]
    UnknownInvite,
    /// 400 `90271`: the invite ended (declined, cancelled, expired) before this answer.
    #[error("This invite is no longer open")]
    InviteClosed,
    /// 429 `90003`: a limit; try again after this many seconds.
    #[error("Too many invites. Try again in {retry_after_secs} s")]
    RateLimited { retry_after_secs: u64 },
    /// 400 `50035`, or refused here before it was sent.
    #[error("tether refused it: {0}")]
    Invalid(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
