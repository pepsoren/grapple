mod common;

use common::*;
use grapple_launch::epic_auth::{EpicApi, EpicAuth, OAUTH_BASE};
use grapple_launch::LaunchError;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn user_json(access_expires: &str) -> String {
    format!(
        r#"{{
  "access_token": "OLD-ACCESS",
  "account_id": "acc123",
  "displayName": "Tester",
  "expires_at": "{access_expires}",
  "refresh_token": "OLD-REFRESH",
  "refresh_expires_at": "2099-01-01T00:00:00.000Z",
  "extra_field_to_keep": {{"a": [1, 2, 3]}},
  "client_id": "someclient"
}}"#
    )
}

fn api(base: &str) -> EpicApi {
    EpicApi { oauth_base: base.to_string(), ecommerce_base: base.to_string(), ..EpicApi::default() }
}

fn token_response() -> String {
    r#"{"access_token":"NEW-ACCESS","refresh_token":"NEW-REFRESH","expires_at":"2099-01-01T00:00:00.000Z","account_id":"acc123","displayName":"Tester","token_type":"bearer"}"#.into()
}

#[tokio::test]
async fn valid_token_needs_no_refresh_and_leaves_the_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2099-01-01T00:00:00.000Z"));
    let before = std::fs::read(&uj).unwrap();

    let server = MockServer::start(|r| match r.path.as_str() {
        "/account/api/oauth/exchange" => (200, r#"{"code":"EXCHANGE-1"}"#.into()),
        _ => (404, "{}".into()),
    })
    .await;
    let auth = EpicAuth::new(api(&server.base), uj.clone()).unwrap();
    let t = auth.game_token().await.unwrap();
    assert_eq!(t.code.expose(), "EXCHANGE-1");
    assert_eq!(t.identity.account_id, "acc123");
    assert_eq!(t.identity.display_name, "Tester");

    let seen = server.seen();
    assert_eq!(seen.len(), 1, "only the exchange call");
    assert_eq!(seen[0].method, "GET");
    assert_eq!(seen[0].headers["authorization"], "Bearer OLD-ACCESS");
    assert!(seen[0].headers["user-agent"].starts_with("UELauncher/11.0.1"));
    assert_eq!(std::fs::read(&uj).unwrap(), before, "user.json untouched");
    assert!(!format!("{t:?}").contains("EXCHANGE-1"), "Debug must not leak the code");
}

#[tokio::test]
async fn expiring_token_is_refreshed_and_written_back_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    // 5 minutes left: inside the 10 minute margin
    let soon = (chrono::Utc::now() + chrono::Duration::minutes(5)).format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
    write(&uj, &user_json(&soon));
    #[cfg(unix)]
    std::fs::set_permissions(&uj, std::fs::Permissions::from_mode(0o644)).unwrap();

    let server = MockServer::start(|r| match r.path.as_str() {
        "/account/api/oauth/token" => (200, token_response()),
        "/account/api/oauth/exchange" => (200, r#"{"code":"EXCHANGE-2"}"#.into()),
        _ => (404, "{}".into()),
    })
    .await;
    let auth = EpicAuth::new(api(&server.base), uj.clone()).unwrap();
    let t = auth.game_token().await.unwrap();
    assert_eq!(t.code.expose(), "EXCHANGE-2");

    let seen = server.seen();
    assert_eq!(seen[0].path, "/account/api/oauth/token");
    assert_eq!(seen[0].method, "POST");
    // basic auth with legendary's client id/secret: base64("34a02cf8...:daafbccc...")
    assert_eq!(seen[0].headers["authorization"], "Basic MzRhMDJjZjhmNDQxNGUyOWIxNTkyMTg3NmRhMzZmOWE6ZGFhZmJjY2M3Mzc3NDUwMzlkZmZlNTNkOTRmYzc2Y2Y=");
    assert!(seen[0].body.contains("grant_type=refresh_token"));
    assert!(seen[0].body.contains("refresh_token=OLD-REFRESH"));
    assert!(seen[0].body.contains("token_type=eg1"));
    assert_eq!(seen[1].path, "/account/api/oauth/exchange");
    assert_eq!(seen[1].headers["authorization"], "Bearer NEW-ACCESS", "exchange uses the refreshed token");

    // written back: new tokens, every other field preserved, mode 0600, no temp files
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&uj).unwrap()).unwrap();
    assert_eq!(v["access_token"], "NEW-ACCESS");
    assert_eq!(v["refresh_token"], "NEW-REFRESH");
    assert_eq!(v["extra_field_to_keep"]["a"][2], 3);
    assert_eq!(v["client_id"], "someclient");
    assert_eq!(v["refresh_expires_at"], "2099-01-01T00:00:00.000Z");
    #[cfg(unix)]
    assert_eq!(std::fs::metadata(&uj).unwrap().permissions().mode() & 0o777, 0o600);
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
}

#[tokio::test]
async fn rejected_access_token_triggers_one_refresh_and_retry() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2099-01-01T00:00:00.000Z")); // looks valid, server disagrees
    let server = MockServer::start(|r| match (r.path.as_str(), r.headers.get("authorization").map(String::as_str)) {
        ("/account/api/oauth/exchange", Some("Bearer OLD-ACCESS")) => (401, r#"{"errorCode":"x"}"#.into()),
        ("/account/api/oauth/exchange", Some("Bearer NEW-ACCESS")) => (200, r#"{"code":"EXCHANGE-3"}"#.into()),
        ("/account/api/oauth/token", _) => (200, token_response()),
        _ => (404, "{}".into()),
    })
    .await;
    let auth = EpicAuth::new(api(&server.base), uj.clone()).unwrap();
    let t = auth.game_token().await.unwrap();
    assert_eq!(t.code.expose(), "EXCHANGE-3");
    let paths: Vec<String> = server.seen().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/account/api/oauth/exchange", "/account/api/oauth/token", "/account/api/oauth/exchange"]);
}

#[tokio::test]
async fn invalid_refresh_token_means_signed_out_and_the_file_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2000-01-01T00:00:00.000Z"));
    let before = std::fs::read(&uj).unwrap();
    let server = MockServer::start(|_| (400, r#"{"errorCode":"errors.com.epicgames.account.auth_token.invalid_refresh_token","errorMessage":"nope"}"#.into())).await;
    let auth = EpicAuth::new(api(&server.base), uj.clone()).unwrap();
    let err = auth.game_token().await.unwrap_err();
    assert!(matches!(err, LaunchError::EpicSignedOut(_)), "{err:?}");
    assert!(!err.to_string().contains("OLD-REFRESH"));
    assert_eq!(std::fs::read(&uj).unwrap(), before);
}

#[tokio::test]
async fn unreachable_epic_is_a_network_error_so_offline_fallback_can_apply() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2000-01-01T00:00:00.000Z"));
    // nothing listens on port 1
    let auth = EpicAuth::new(api("http://127.0.0.1:1"), uj).unwrap();
    let err = auth.game_token().await.unwrap_err();
    assert!(matches!(err, LaunchError::Network(_)), "{err:?}");
}

#[tokio::test]
async fn server_error_on_refresh_is_a_network_error() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2000-01-01T00:00:00.000Z"));
    let server = MockServer::start(|_| (503, "{}".into())).await;
    let auth = EpicAuth::new(api(&server.base), uj).unwrap();
    assert!(matches!(auth.game_token().await.unwrap_err(), LaunchError::Network(_)));
}

#[tokio::test]
async fn missing_login_file_says_to_sign_in() {
    let dir = tempfile::tempdir().unwrap();
    let auth = EpicAuth::new(api("http://127.0.0.1:1"), dir.path().join("user.json")).unwrap();
    assert!(matches!(auth.game_token().await.unwrap_err(), LaunchError::EpicSignedOut(_)));
}

#[tokio::test]
async fn ownership_token_request_matches_legendary() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2099-01-01T00:00:00.000Z"));
    let server = MockServer::start(|r| match r.path.as_str() {
        "/account/api/oauth/exchange" => (200, r#"{"code":"E"}"#.into()),
        p if p.ends_with("/ownershipToken") => (200, "OVT-BYTES".into()),
        _ => (404, "{}".into()),
    })
    .await;
    let auth = EpicAuth::new(api(&server.base), uj).unwrap();
    let t = auth.game_token().await.unwrap();
    let ovt = auth.ownership_token(&t.identity, &t.access, "ns1", "cat1").await.unwrap();
    assert_eq!(ovt, b"OVT-BYTES");
    let last = server.seen().pop().unwrap();
    assert_eq!(last.method, "POST");
    assert_eq!(last.path, "/ecommerceintegration/api/public/platforms/EPIC/identities/acc123/ownershipToken");
    assert!(last.body.contains("nsCatalogItemId=ns1%3Acat1"), "{}", last.body);
}

#[test]
fn production_hosts_are_legendarys() {
    assert!(OAUTH_BASE.contains("account-public-service-prod03.ol.epicgames.com"));
}

#[tokio::test]
async fn exchange_code_for_the_store_window_refreshes_and_writes_back_like_a_launch() {
    let dir = tempfile::tempdir().unwrap();
    let uj = dir.path().join("user.json");
    write(&uj, &user_json("2000-01-01T00:00:00.000Z")); // expired: must refresh first
    let server = MockServer::start(|r| match r.path.as_str() {
        "/account/api/oauth/token" => (200, token_response()),
        "/account/api/oauth/exchange" => (200, r#"{"code":"STORE-CODE"}"#.into()),
        _ => (404, "{}".into()),
    })
    .await;
    let auth = EpicAuth::new(api(&server.base), uj.clone()).unwrap();
    let code = auth.exchange_code().await.unwrap();
    assert_eq!(code.expose(), "STORE-CODE");
    assert!(!format!("{code:?} {code}").contains("STORE-CODE"), "the code is redacted");
    let seen = server.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].headers["authorization"], "Bearer NEW-ACCESS");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&uj).unwrap()).unwrap();
    assert_eq!(v["refresh_token"], "NEW-REFRESH", "rotated refresh token written back");

    // `session` reuses the now valid token without another refresh.
    let (who, access) = auth.session().await.unwrap();
    assert_eq!(who.account_id, "acc123");
    assert_eq!(access.expose(), "NEW-ACCESS");
    assert_eq!(server.seen().len(), 2, "no network for a valid token");
}
