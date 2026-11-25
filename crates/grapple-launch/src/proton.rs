//! The Proton versions grapple can run an Epic game with (Linux): Heroic's (`~/.config/heroic/tools/proton/*`),
//! the ones in Steam's `compatibilitytools.d` (where umu keeps its GE-Proton), and Steam's own
//! (`steamapps/common/Proton*` in every library). Read-only: nothing is downloaded.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// The name shown for "the newest GE-Proton found", the default when there is one.
pub const LATEST_GE: &str = "GE-Proton (latest)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonVersion {
    /// The folder name ("GE-Proton10-25", "Proton 9.0 (Beta)").
    pub name: String,
    /// The `proton` script in it.
    pub bin: PathBuf,
}

fn scan_dir(dir: &Path, only_proton_prefix: bool, out: &mut Vec<ProtonVersion>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
        if only_proton_prefix && !name.starts_with("Proton") {
            continue;
        }
        let bin = e.path().join("proton");
        if bin.is_file() && !out.iter().any(|p| p.name == name) {
            out.push(ProtonVersion { name, bin });
        }
    }
}

/// Every Proton found, newest-looking first.
pub fn find(heroic_dir: &Path, steam_roots: &[PathBuf], libraries: &[PathBuf]) -> Vec<ProtonVersion> {
    let mut out = Vec::new();
    scan_dir(&heroic_dir.join("tools").join("proton"), false, &mut out);
    for root in steam_roots {
        scan_dir(&root.join("compatibilitytools.d"), false, &mut out);
    }
    for lib in libraries {
        scan_dir(&lib.join("steamapps").join("common"), true, &mut out);
    }
    out.sort_by(|a, b| natural(&b.name, &a.name));
    out
}

/// Compare names with their numbers as numbers ("GE-Proton10-2" > "GE-Proton9-27").
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let nb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let va: u64 = std::str::from_utf8(&a[..na]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                let vb: u64 = std::str::from_utf8(&b[..nb]).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
                match va.cmp(&vb) {
                    Ordering::Equal => {
                        a = &a[na..];
                        b = &b[nb..];
                    }
                    o => return o,
                }
            }
            (Some(x), Some(y)) => match x.cmp(y) {
                Ordering::Equal => {
                    a = &a[1..];
                    b = &b[1..];
                }
                o => return o,
            },
        }
    }
}

/// The newest GE-Proton.
pub fn latest_ge(list: &[ProtonVersion]) -> Option<&ProtonVersion> {
    list.iter().filter(|p| p.name.starts_with("GE-Proton")).max_by(|a, b| natural(&a.name, &b.name))
}

/// The `proton` script for a stored choice ([`LATEST_GE`] or a folder name).
pub fn resolve(name: &str, list: &[ProtonVersion]) -> Option<PathBuf> {
    if name == LATEST_GE {
        return latest_ge(list).map(|p| p.bin.clone());
    }
    list.iter().find(|p| p.name == name).map(|p| p.bin.clone())
}

/// What the install sheet offers: "GE-Proton (latest)" first and default when a GE-Proton exists, then every
/// version; without GE-Proton the default is Heroic's own choice (`heroic_default`, a folder name) or the first.
pub fn choices(list: &[ProtonVersion], heroic_default: Option<&str>) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let has_ge = latest_ge(list).is_some();
    if has_ge {
        out.push((LATEST_GE.to_string(), true));
    }
    let default = if has_ge { None } else { heroic_default.filter(|d| list.iter().any(|p| p.name == *d)).or(list.first().map(|p| p.name.as_str())) };
    for p in list {
        out.push((p.name.clone(), Some(p.name.as_str()) == default));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["GE-Proton9-27", "GE-Proton10-2", "GE-Proton10-25", "GE-Proton8-32"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, ["GE-Proton8-32", "GE-Proton9-27", "GE-Proton10-2", "GE-Proton10-25"]);
    }

    #[test]
    fn finds_resolves_and_offers() {
        let d = tempfile::tempdir().unwrap();
        let mk = |p: &str| {
            let f = d.path().join(p).join("proton");
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
        };
        mk("heroic/tools/proton/GE-Proton9-27");
        mk("heroic/tools/proton/GE-Proton10-25");
        mk("steam/compatibilitytools.d/UMU-Proton-9.0-3");
        mk("lib/steamapps/common/Proton 9.0 (Beta)");
        std::fs::create_dir_all(d.path().join("lib/steamapps/common/Rust")).unwrap();
        let list = find(&d.path().join("heroic"), &[d.path().join("steam")], &[d.path().join("lib")]);
        assert_eq!(list.len(), 4);
        assert_eq!(resolve(LATEST_GE, &list).unwrap(), d.path().join("heroic/tools/proton/GE-Proton10-25/proton"));
        assert_eq!(resolve("Proton 9.0 (Beta)", &list).unwrap(), d.path().join("lib/steamapps/common/Proton 9.0 (Beta)/proton"));
        assert!(resolve("nope", &list).is_none());
        let c = choices(&list, None);
        assert_eq!(c[0], (LATEST_GE.to_string(), true));
        assert_eq!(c.iter().filter(|x| x.1).count(), 1);
        let no_ge: Vec<ProtonVersion> = list.into_iter().filter(|p| !p.name.starts_with("GE")).collect();
        let c = choices(&no_ge, Some("UMU-Proton-9.0-3"));
        assert_eq!(c.iter().find(|x| x.1).unwrap().0, "UMU-Proton-9.0-3");
    }
}
