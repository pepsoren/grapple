//! Test helpers: a writer for Steam's binary appinfo.vdf (v27/v28/v29) and a fixture tree copier.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub enum Kv {
    Section(Vec<(String, Kv)>),
    Str(String),
    I32(i32),
    U64(u64),
    F32(f32),
    Wide(String),
}

pub fn section(items: Vec<(&str, Kv)>) -> Kv {
    Kv::Section(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub fn s(v: &str) -> Kv {
    Kv::Str(v.to_string())
}

/// `appinfo { appid, common { name, type, clienticon, <noise> }, config { .. } }`
pub fn app_entry(appid: u32, name: &str, kind: &str, icon: Option<&str>) -> (u32, Kv) {
    let mut common = vec![
        ("name", s(name)),
        ("noise_int", Kv::I32(-5)),
        ("noise_section", section(vec![("deep", section(vec![("x", s("y"))])), ("w", Kv::Wide("wide ✓".into()))])),
        ("type", s(kind)),
        ("noise_u64", Kv::U64(u64::MAX)),
        ("noise_f", Kv::F32(1.5)),
    ];
    if let Some(i) = icon {
        common.push(("clienticon", s(i)));
    }
    let root = section(vec![
        ("appinfo", section(vec![("appid", Kv::I32(appid as i32)), ("common", section(common)), ("config", section(vec![("installdir", s("x"))]))])),
    ]);
    (appid, root)
}

struct Strtab {
    map: HashMap<String, u32>,
    list: Vec<String>,
}

fn write_kv(out: &mut Vec<u8>, key: &str, v: &Kv, tab: &mut Strtab, v29: bool) {
    let ty: u8 = match v {
        Kv::Section(_) => 0,
        Kv::Str(_) => 1,
        Kv::I32(_) => 2,
        Kv::F32(_) => 3,
        Kv::Wide(_) => 5,
        Kv::U64(_) => 7,
    };
    out.push(ty);
    if v29 {
        let next = tab.list.len() as u32;
        let idx = *tab.map.entry(key.to_string()).or_insert_with(|| {
            tab.list.push(key.to_string());
            next
        });
        out.extend(idx.to_le_bytes());
    } else {
        out.extend(key.as_bytes());
        out.push(0);
    }
    match v {
        Kv::Section(items) => {
            for (k, c) in items {
                write_kv(out, k, c, tab, v29);
            }
            out.push(8);
        }
        Kv::Str(x) => {
            out.extend(x.as_bytes());
            out.push(0);
        }
        Kv::I32(x) => out.extend(x.to_le_bytes()),
        Kv::F32(x) => out.extend(x.to_le_bytes()),
        Kv::U64(x) => out.extend(x.to_le_bytes()),
        Kv::Wide(x) => {
            for u in x.encode_utf16() {
                out.extend(u.to_le_bytes());
            }
            out.extend([0, 0]);
        }
    }
}

/// `version` is 27, 28 or 29. Each app is `(appid, root KeyValues)` where root is the unnamed top-level object.
pub fn build_appinfo(version: u32, apps: &[(u32, Kv)]) -> Vec<u8> {
    let v29 = version == 29;
    let mut tab = Strtab { map: HashMap::new(), list: Vec::new() };
    let mut out = Vec::new();
    out.extend((0x0756_4400u32 + (version / 10) * 16 + version % 10).to_le_bytes());
    out.extend(1u32.to_le_bytes()); // universe
    if v29 {
        out.extend(0i64.to_le_bytes()); // string table offset, patched below
    }
    for (appid, root) in apps {
        let mut body = Vec::new();
        body.extend(2u32.to_le_bytes()); // infoState
        body.extend(1_700_000_000u32.to_le_bytes()); // lastUpdated
        body.extend(0u64.to_le_bytes()); // token
        body.extend([0xAA; 20]); // sha1
        body.extend(77u32.to_le_bytes()); // changeNumber
        if version >= 28 {
            body.extend([0xBB; 20]); // binary sha1
        }
        // The root object's children, then its terminator.
        if let Kv::Section(items) = root {
            for (k, c) in items {
                write_kv(&mut body, k, c, &mut tab, v29);
            }
        }
        body.push(8);
        out.extend(appid.to_le_bytes());
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
    }
    out.extend(0u32.to_le_bytes()); // end marker
    if v29 {
        let so = out.len() as i64;
        out[8..16].copy_from_slice(&so.to_le_bytes());
        out.extend((tab.list.len() as u32).to_le_bytes());
        for k in &tab.list {
            out.extend(k.as_bytes());
            out.push(0);
        }
    }
    out
}

/// Copy `src` into `dst`, replacing `@ROOT@` with `root` inside text files.
pub fn copy_fixture(src: &Path, dst: &Path, root: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_fixture(&e.path(), &to, root);
        } else {
            let text = std::fs::read_to_string(e.path()).unwrap();
            // Forward slashes: the fixtures are JSON and VDF, where a Windows `\` would be an escape.
            std::fs::write(to, text.replace("@ROOT@", &root.to_str().unwrap().replace('\\', "/"))).unwrap();
        }
    }
}

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}
