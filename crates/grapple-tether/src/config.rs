//! Which tether instance to talk to.
//!
//! The address comes from Grapple's settings (key [`INSTANCE_SETTING_KEY`] in the `settings`
//! table, a string such as `https://tether.example`). Until it is set, the app uses the tether
//! desktop app's chosen host if it has one, else [`DEFAULT_INSTANCE_URL`], the official tether.
//! Development points the setting at a local `pnpm dev:lite` (`http://localhost:5173`).

use crate::error::{Error, Result};
use std::net::IpAddr;
use url::Url;

/// The default instance: the official tether (Josh's, 2026-10-18).
pub const DEFAULT_INSTANCE_URL: &str = "https://app.tethr.lol";

/// Grapple's setting that holds the instance address (`Db::setting::<String>`).
pub const INSTANCE_SETTING_KEY: &str = "tether.instance_url";

/// A checked tether instance: an origin only (`scheme://host[:port]`), `https`, or `http` for a
/// loopback address or a Tailscale address (100.64.0.0/10, `*.ts.net`), whose traffic is already
/// encrypted by WireGuard. Tokens are never sent over plain HTTP anywhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TetherConfig {
    origin: String,
}

fn plain_http_ok(host: &url::Host<&str>) -> bool {
    match host {
        url::Host::Domain(d) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d.ends_with(".ts.net")
        }
        url::Host::Ipv4(ip) => {
            ip.is_loopback() || (ip.octets()[0] == 100 && (ip.octets()[1] & 0xc0) == 64)
        }
        url::Host::Ipv6(ip) => IpAddr::V6(*ip).is_loopback(),
    }
}

impl TetherConfig {
    pub fn new(address: &str) -> Result<Self> {
        let bad = |why: &str| Error::InvalidInstance(format!("{address}: {why}"));
        let url = Url::parse(address.trim()).map_err(|_| bad("not a URL"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad("no user name or password in the address"));
        }
        if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
            return Err(bad("use just the address, like https://tether.example"));
        }
        let host = url.host().ok_or_else(|| bad("no host"))?;
        match url.scheme() {
            "https" => {}
            "http" if plain_http_ok(&host) => {}
            "http" => {
                return Err(bad(
                    "use https (plain http only on this computer or a tailnet)",
                ));
            }
            _ => return Err(bad("use https")),
        }
        let origin = url.origin().ascii_serialization();
        Ok(TetherConfig { origin })
    }

    /// From Grapple's setting (`None` or empty: [`DEFAULT_INSTANCE_URL`]).
    pub fn from_setting(value: Option<&str>) -> Result<Self> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => TetherConfig::new(v),
            None => TetherConfig::new(DEFAULT_INSTANCE_URL),
        }
    }

    /// `scheme://host[:port]`, no trailing slash.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// An API route: `<origin>/api/v1<path>`.
    pub fn api(&self, path: &str) -> String {
        format!("{}/api/v1{}", self.origin, path)
    }

    /// The keyring entry's user name: one entry per instance.
    pub fn keyring_user(&self) -> String {
        format!("tether:{}", self.origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_origins() {
        assert_eq!(
            TetherConfig::new("https://tethr.lol").unwrap().origin(),
            "https://tethr.lol"
        );
        assert_eq!(
            TetherConfig::new("https://box.tail1234.ts.net:8443/")
                .unwrap()
                .api("/gateway"),
            "https://box.tail1234.ts.net:8443/api/v1/gateway"
        );
        assert!(TetherConfig::new("http://localhost:5173").is_ok());
        assert!(TetherConfig::new("http://127.0.0.1:3001").is_ok());
        assert!(TetherConfig::new("http://100.101.102.103:5173").is_ok());
        assert!(TetherConfig::new("http://box.tail1234.ts.net").is_ok());
    }

    #[test]
    fn refuses_unsafe_or_odd_addresses() {
        for bad in [
            "http://tether.example",
            "http://192.168.1.10:5173",
            "ftp://tether.example",
            "https://tether.example/app",
            "https://tether.example/?x=1",
            "https://user:pw@tether.example",
            "not a url",
        ] {
            assert!(TetherConfig::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_is_the_official_tether() {
        assert_eq!(
            TetherConfig::from_setting(None).unwrap().origin(),
            "https://app.tethr.lol"
        );
        assert_eq!(
            TetherConfig::from_setting(Some(" ")).unwrap().origin(),
            "https://app.tethr.lol"
        );
    }
}
