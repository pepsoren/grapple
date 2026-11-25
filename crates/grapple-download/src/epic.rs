//! Epic's launcher service (owned games, where a build's manifest is) and the chunk CDN.
//!
//! Service calls carry `Authorization: bearer <token>` with a token from [`TokenSource`] (in the app:
//! `EpicAuth::session()`); chunk downloads carry no auth header (verified: the three CDNs answer without one).
//! Nothing here logs a token or a URL with query parameters.

use crate::error::{Error, Result};
use crate::manifest::{hex, sha1};
use futures_util::future::BoxFuture;
use grapple_launch::epic_auth::USER_AGENT;
use grapple_launch::plan::Secret;
use serde::Deserialize;
use std::time::Duration;

pub const LAUNCHER_BASE: &str = "https://launcher-public-service-prod06.ol.epicgames.com";

/// Where an Epic access token comes from. The app implements it with `EpicAuth::session()`.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> BoxFuture<'_, Result<Secret>>;
}

impl TokenSource for grapple_launch::epic_auth::EpicAuth {
    fn token(&self) -> BoxFuture<'_, Result<Secret>> {
        Box::pin(async move { Ok(self.session().await?.1) })
    }
}

/// One owned game's current build (`/assets/Windows?label=Live`, or legendary's `assets.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    #[serde(rename = "appName", alias = "app_name")]
    pub app_name: String,
    #[serde(rename = "labelName", alias = "label_name", default)]
    pub label_name: String,
    #[serde(rename = "buildVersion", alias = "build_version")]
    pub build_version: String,
    #[serde(rename = "catalogItemId", alias = "catalog_item_id", default)]
    pub catalog_item_id: String,
    #[serde(default)]
    pub namespace: String,
    #[serde(rename = "assetId", alias = "asset_id", default)]
    pub asset_id: String,
}

/// Where to get a build's manifest, and the chunk base URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLocation {
    pub build_version: String,
    /// SHA-1 (hex) of the manifest file.
    pub hash: String,
    pub manifest_urls: Vec<String>,
    /// Every distinct manifest URL without its last path part, in order.
    pub base_urls: Vec<String>,
}

/// Ids that go into a URL path: letters, digits, `_`, `-` only.
pub fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[derive(Clone)]
pub struct EpicClient {
    pub launcher_base: String,
    http: reqwest::Client,
}

impl EpicClient {
    pub fn new(launcher_base: impl Into<String>) -> Result<EpicClient> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()?;
        Ok(EpicClient { launcher_base: launcher_base.into().trim_end_matches('/').to_string(), http })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    fn check(status: reqwest::StatusCode) -> Result<()> {
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::SignedOut);
        }
        if !status.is_success() {
            return Err(Error::Network(format!("Epic answered {status}")));
        }
        Ok(())
    }

    /// Every owned game with its current Live build.
    pub async fn assets(&self, token: &Secret) -> Result<Vec<Asset>> {
        let resp = self
            .http
            .get(format!("{}/launcher/api/public/assets/Windows?label=Live", self.launcher_base))
            .bearer_auth(token.expose())
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        Self::check(resp.status())?;
        Ok(resp.json().await?)
    }

    pub async fn manifest_location(&self, token: &Secret, namespace: &str, catalog_item_id: &str, app_name: &str) -> Result<ManifestLocation> {
        if ![namespace, catalog_item_id, app_name].iter().all(|s| valid_id(s)) {
            return Err(Error::Invalid("not an Epic game id".into()));
        }
        let url = format!(
            "{}/launcher/api/public/assets/v2/platform/Windows/namespace/{namespace}/catalogItem/{catalog_item_id}/app/{app_name}/label/Live",
            self.launcher_base
        );
        let resp = self.http.get(url).bearer_auth(token.expose()).timeout(Duration::from_secs(30)).send().await?;
        Self::check(resp.status())?;
        let body: serde_json::Value = resp.json().await?;
        parse_location(&body)
    }

    /// The manifest from the first URL that answers with the right bytes (SHA-1 = the location's hash).
    pub async fn manifest(&self, loc: &ManifestLocation) -> Result<Vec<u8>> {
        let mut last = Error::Network("no manifest URL".into());
        for url in &loc.manifest_urls {
            match self.http.get(url).timeout(Duration::from_secs(60)).send().await {
                Ok(r) if r.status().is_success() => match r.bytes().await {
                    Ok(b) if loc.hash.is_empty() || hex(&sha1(&b)).eq_ignore_ascii_case(&loc.hash) => return Ok(b.to_vec()),
                    Ok(_) => last = Error::Format("manifest hash does not match".into()),
                    Err(e) => last = e.into(),
                },
                Ok(r) => last = Error::Network(format!("manifest: {}", r.status())),
                Err(e) => last = e.into(),
            }
        }
        Err(last)
    }
}

/// `{"elements":[{"buildVersion","hash","useSignedUrl","manifests":[{"uri","queryParams":[{"name","value"}]}]}]}`
pub fn parse_location(body: &serde_json::Value) -> Result<ManifestLocation> {
    let el = body.pointer("/elements/0").ok_or_else(|| Error::Network("Epic did not say where the game's files are".into()))?;
    if el.get("useSignedUrl").and_then(serde_json::Value::as_bool).unwrap_or(false) {
        return Err(Error::SignedUrl);
    }
    let text = |k: &str| el.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
    let mut manifest_urls = Vec::new();
    let mut base_urls: Vec<String> = Vec::new();
    for m in el.get("manifests").and_then(serde_json::Value::as_array).into_iter().flatten() {
        let Some(uri) = m.get("uri").and_then(serde_json::Value::as_str) else { continue };
        if !(uri.starts_with("https://") || uri.starts_with("http://")) {
            continue;
        }
        let params: Vec<String> = m
            .get("queryParams")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|q| Some(format!("{}={}", q.get("name")?.as_str()?, q.get("value")?.as_str()?)))
            .collect();
        manifest_urls.push(if params.is_empty() { uri.to_string() } else { format!("{uri}?{}", params.join("&")) });
        if let Some((base, _)) = uri.rsplit_once('/')
            && !base_urls.iter().any(|b| b == base)
        {
            base_urls.push(base.to_string());
        }
    }
    if manifest_urls.is_empty() {
        return Err(Error::Network("Epic gave no manifest URL".into()));
    }
    Ok(ManifestLocation { build_version: text("buildVersion"), hash: text("hash"), manifest_urls, base_urls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn location_with_and_without_query() {
        let body = json!({"elements": [{"appName": "Sugar", "buildVersion": "v2", "hash": "ab", "useSignedUrl": false, "manifests": [
            {"uri": "https://a.test/Builds/Org/x/default/m.manifest", "queryParams": [{"name": "f_token", "value": "t"}, {"name": "x", "value": "1"}]},
            {"uri": "https://b.test/Builds/Org/x/default/m.manifest", "queryParams": []},
            {"uri": "https://a.test/Builds/Org/x/default/m.manifest"}
        ]}]});
        let l = parse_location(&body).unwrap();
        assert_eq!(l.manifest_urls[0], "https://a.test/Builds/Org/x/default/m.manifest?f_token=t&x=1");
        assert_eq!(l.manifest_urls[1], "https://b.test/Builds/Org/x/default/m.manifest");
        assert_eq!(l.base_urls, ["https://a.test/Builds/Org/x/default", "https://b.test/Builds/Org/x/default"]);
        assert_eq!(l.build_version, "v2");
        let signed = json!({"elements": [{"useSignedUrl": true, "manifests": [{"uri": "https://a.test/m.manifest"}]}]});
        assert!(matches!(parse_location(&signed), Err(Error::SignedUrl)));
        assert!(parse_location(&json!({"elements": []})).is_err());
    }

    #[test]
    fn ids() {
        assert!(valid_id("0a2d9f6403244d12969e11da6713137b"));
        assert!(valid_id("Sugar"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(""));
    }
}
