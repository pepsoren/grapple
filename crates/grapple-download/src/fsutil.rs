//! Small cross-platform file helpers: free space, positional writes, safe relative paths, drives and labels.
//! Unix uses `statvfs` and the device number; Windows uses `GetDiskFreeSpaceExW`, `GetVolumeInformationW` and the
//! drive prefix (type-checked from Linux with `nix develop .#windows`, built and tested by the Windows CI job).

use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Space {
    pub free: u64,
    pub total: u64,
}

/// Free space lookup; tests inject a fake.
pub type FreeSpaceFn = Arc<dyn Fn(&Path) -> io::Result<Space> + Send + Sync>;

pub fn real_free_space() -> FreeSpaceFn {
    Arc::new(|p: &Path| free_space(p))
}

/// The nearest ancestor of `p` that exists (a folder about to be created reports its drive's space).
pub fn existing_ancestor(p: &Path) -> Option<PathBuf> {
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.exists() {
            return Some(c.to_path_buf());
        }
        cur = c.parent();
    }
    None
}

#[cfg(unix)]
pub fn free_space(p: &Path) -> io::Result<Space> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let at = existing_ancestor(p).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such folder"))?;
    let c = CString::new(at.as_os_str().as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path has a NUL"))?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a zeroed out-parameter of the right type.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let unit = s.f_frsize as u64;
    Ok(Space { free: s.f_bavail as u64 * unit, total: s.f_blocks as u64 * unit })
}

#[cfg(windows)]
pub fn free_space(p: &Path) -> io::Result<Space> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let at = existing_ancestor(p).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such folder"))?;
    let wide: Vec<u16> = at.as_os_str().encode_wide().chain([0]).collect();
    let (mut free, mut total, mut _all) = (0u64, 0u64, 0u64);
    // SAFETY: a NUL-terminated wide string and valid out-pointers.
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut _all) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Space { free, total })
}

#[cfg(not(any(unix, windows)))]
pub fn free_space(_p: &Path) -> io::Result<Space> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "free space is not known on this system"))
}

/// Write all of `buf` at `offset` without moving a shared cursor (many chunks write into one file).
#[cfg(unix)]
pub fn write_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, offset)
}

#[cfg(unix)]
pub fn read_at(f: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, offset)
}

#[cfg(windows)]
pub fn read_at(f: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "file ended early"));
        }
        buf = &mut buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(windows)]
pub fn write_at(f: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, offset)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0"));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

/// A manifest file name as a relative path: no root, no drive, no `..`, no empty or `.` parts. `None` for anything
/// that could leave the install folder.
pub fn safe_rel(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.is_empty() || name.starts_with('/') || name.contains('\0') {
        return None;
    }
    let mut out = PathBuf::new();
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    out.components().all(|c| matches!(c, Component::Normal(_))).then_some(out)
}

/// Open a game file for writing without following a link at the file itself, so a link can never redirect a
/// write out of the install folder ([`check_no_links`] covers the folders above it). Unix only; Windows opens
/// normally (creating links there needs extra rights, and the downloader never makes them).
pub fn open_game_file(p: &Path, create: bool) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    if create {
        o.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    o.open(p)
}

/// Fails when a folder between `root` and `root/rel` is a symlink, so a write can never be redirected out of the
/// install folder by a link an earlier build (or anyone) put there.
pub fn check_no_links(root: &Path, rel: &Path) -> io::Result<()> {
    let mut cur = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for c in &parts[..parts.len().saturating_sub(1)] {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{} is a link", cur.display())));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Total size of the regular files under `dir` (Steam's `downloading/<id>` folder).
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                total += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

#[cfg(unix)]
pub fn set_executable(p: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_mode(perm.mode() | 0o111);
    std::fs::set_permissions(p, perm)
}

#[cfg(not(unix))]
pub fn set_executable(_p: &Path) -> io::Result<()> {
    Ok(())
}

/// Which filesystem a path is on: the device number on unix, the drive prefix on Windows.
#[cfg(unix)]
pub fn device(p: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(existing_ancestor(p)?).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
pub fn device(p: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let prefix = existing_ancestor(p)?.components().next().map(|c| c.as_os_str().to_ascii_uppercase())?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    prefix.hash(&mut h);
    Some(h.finish())
}

/// The top folder of the filesystem `p` is on (`/data` for `/data/SteamLibrary`; `C:\` on Windows).
pub fn mount_point(p: &Path) -> Option<PathBuf> {
    let start = existing_ancestor(p)?;
    let dev = device(&start)?;
    let mut top = start.clone();
    let mut cur = start.parent();
    while let Some(c) = cur {
        if device(c) != Some(dev) {
            break;
        }
        top = c.to_path_buf();
        cur = c.parent();
    }
    Some(top)
}

/// "Home drive" for the filesystem the home folder is on, else "<Name> drive" from the mount point's last part
/// (`/data` → "Data drive"); "System drive" for `/`. Windows: the drive letter and volume label ("D: Games").
pub fn drive_label(p: &Path, home: &Path) -> String {
    if device(p).is_some() && device(p) == device(home) {
        return "Home drive".into();
    }
    #[cfg(windows)]
    {
        return windows_label(p);
    }
    #[cfg(not(windows))]
    {
        let mp = mount_point(p).unwrap_or_else(|| p.to_path_buf());
        match mp.file_name().and_then(|n| n.to_str()) {
            Some(n) if !n.is_empty() => {
                let mut c = n.chars();
                let first = c.next().map(|f| f.to_uppercase().collect::<String>()).unwrap_or_default();
                format!("{first}{} drive", c.as_str())
            }
            _ => "System drive".into(),
        }
    }
}

#[cfg(windows)]
fn windows_label(p: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let root = mount_point(p).unwrap_or_else(|| p.to_path_buf());
    let letter = root.to_string_lossy().trim_end_matches('\\').to_string();
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain([0]).collect();
    let mut name = [0u16; 261];
    // SAFETY: NUL-terminated root path, a buffer with its length, null for the outputs we do not need.
    let ok = unsafe {
        GetVolumeInformationW(wide.as_ptr(), name.as_mut_ptr(), name.len() as u32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0)
    };
    let label = if ok != 0 { String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap_or(0)]) } else { String::new() };
    if label.is_empty() { format!("{letter} drive") } else { format!("{letter} {label}") }
}

/// `~/…` for paths under the home folder (what the UI shows); [`expand_home`] turns it back.
pub fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() && home != Path::new("/") => {
            if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.to_string_lossy()) }
        }
        _ => p.to_string_lossy().into_owned(),
    }
}

pub fn expand_home(s: &str, home: &Path) -> PathBuf {
    if s == "~" {
        home.to_path_buf()
    } else if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(s)
    }
}

/// A folder grapple may delete as a whole (a game folder): absolute, at least two levels deep, not the home
/// folder or one of its parents, and not a link.
pub fn deletable_game_dir(p: &Path, home: &Path) -> bool {
    p.is_absolute()
        && p.components().filter(|c| matches!(c, Component::Normal(_))).count() >= 2
        && !home.starts_with(p)
        && std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn game_files_never_follow_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let link = tmp.path().join("game.bin");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(open_game_file(&link, true).is_err());
        assert!(open_game_file(&link, false).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
        assert!(open_game_file(&tmp.path().join("new.bin"), true).is_ok());
    }

    #[test]
    fn relative_paths_stay_inside() {
        assert_eq!(safe_rel("a/b\\c.txt"), Some(PathBuf::from("a/b/c.txt")));
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a//b", "./a", "C:/x", "a/\0"] {
            assert_eq!(safe_rel(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn tilde_round_trip() {
        let home = Path::new("/home/j");
        assert_eq!(tilde(Path::new("/home/j/.local/share/Steam"), home), "~/.local/share/Steam");
        assert_eq!(tilde(Path::new("/data/Games"), home), "/data/Games");
        assert_eq!(expand_home("~/.local/share/Steam", home), PathBuf::from("/home/j/.local/share/Steam"));
        assert_eq!(expand_home("/data", home), PathBuf::from("/data"));
    }

    #[test]
    fn free_space_of_a_folder_to_be() {
        let d = tempfile::tempdir().unwrap();
        let s = free_space(&d.path().join("not/yet")).unwrap();
        assert!(s.total >= s.free && s.total > 0);
        assert_eq!(drive_label(d.path(), d.path()), "Home drive");
        assert!(mount_point(d.path()).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn links_in_the_way_are_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::os::unix::fs::symlink("/tmp", d.path().join("link")).unwrap();
        assert!(check_no_links(d.path(), Path::new("real/x")).is_ok());
        assert!(check_no_links(d.path(), Path::new("link/x")).is_err());
        assert!(deletable_game_dir(&d.path().join("real"), Path::new("/home/nobody")));
        assert!(!deletable_game_dir(&d.path().join("link"), Path::new("/home/nobody")));
        assert!(!deletable_game_dir(Path::new("/home"), Path::new("/home/j")));
    }
}
