mod common;
use common::*;
use grapple_scan::appinfo::{self, AppInfoError, AppMeta};

fn sample() -> Vec<(u32, Kv)> {
    vec![
        app_entry(10, "Ten", "Game", Some("abc")),
        app_entry(20, "Twenty", "tool", None),
        // An app without a common section.
        (30, section(vec![("appinfo", section(vec![("appid", Kv::I32(30))]))])),
        app_entry(40, "Forty", "game", None),
    ]
}

#[test]
fn parses_all_versions() {
    for version in [27, 28, 29] {
        let data = build_appinfo(version, &sample());
        let m = appinfo::parse(&data, |_| true).unwrap();
        assert_eq!(
            m[&10],
            AppMeta { name: Some("Ten".into()), kind: "game".into(), client_icon: Some("abc".into()) },
            "v{version}"
        );
        assert_eq!(m[&20].kind, "tool");
        assert_eq!(m[&40].name.as_deref(), Some("Forty"));
        assert!(!m.contains_key(&30), "no common section => no meta (v{version})");
        assert_eq!(m.len(), 3);
    }
}

#[test]
fn only_wanted_entries_are_returned() {
    let data = build_appinfo(29, &sample());
    let m = appinfo::parse(&data, |id| id == 40).unwrap();
    assert_eq!(m.keys().copied().collect::<Vec<_>>(), vec![40]);
}

#[test]
fn rejects_garbage_and_truncation_without_panicking() {
    assert_eq!(appinfo::parse(b"", |_| true), Err(AppInfoError::Truncated));
    assert_eq!(appinfo::parse(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], |_| true), Err(AppInfoError::BadMagic(0x04030201)));
    let data = build_appinfo(29, &sample());
    for cut in (0..data.len()).step_by(7) {
        // Any prefix must give Ok (possibly fewer apps) or Err, never a panic.
        let _ = appinfo::parse(&data[..cut], |_| true);
    }
    // Flip bytes in the entry area.
    for i in (16..data.len().min(300)).step_by(3) {
        let mut d = data.clone();
        d[i] ^= 0xFF;
        let _ = appinfo::parse(&d, |_| true);
    }
}
