//! The few Windows registry reads and writes grapple needs (Steam's install path and state, the Run key for
//! autostart, Epic's overlay path), on `windows-sys`. The value decoding is plain Rust and tested everywhere.

/// A registry string as Windows stores it (UTF-16, usually NUL-terminated, sometimes with garbage after the NUL)
/// as a Rust string.
pub fn string_from_wide(units: &[u16]) -> String {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// A `REG_DWORD` from its little-endian bytes (`None` if there are fewer than four).
pub fn dword_from_bytes(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// Steam's folder from its registry values: `HKCU\Software\Valve\Steam\SteamPath` (Steam writes it with forward
/// slashes, often lowercased) first, then `HKLM\SOFTWARE\WOW6432Node\Valve\Steam\InstallPath` (the installer's).
pub fn steam_dir_from(steam_path: Option<&str>, install_path: Option<&str>) -> Option<std::path::PathBuf> {
    [steam_path, install_path]
        .into_iter()
        .flatten()
        .map(|p| p.trim().replace('/', "\\"))
        .find(|p| !p.is_empty())
        .map(std::path::PathBuf::from)
}

/// Steam's folder on this machine (see [`steam_dir_from`]).
#[cfg(windows)]
pub fn steam_dir() -> Option<std::path::PathBuf> {
    let user = read_string(Hive::CurrentUser, "Software\\Valve\\Steam", "SteamPath");
    let machine = read_string(Hive::LocalMachine, "SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath");
    steam_dir_from(user.as_deref(), machine.as_deref())
}

/// Which hive a key lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    CurrentUser,
    LocalMachine,
}

#[cfg(windows)]
mod sys {
    use super::Hive;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
        RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
    };

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn root(h: Hive) -> HKEY {
        match h {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }

    /// A `REG_SZ` (or `REG_EXPAND_SZ`, expanded) value. `None` when the key or value is missing.
    pub fn read_string(hive: Hive, key: &str, value: &str) -> Option<String> {
        let (k, v) = (wide(key), wide(value));
        let mut len: u32 = 0;
        // SAFETY: a size query (null buffer) with valid NUL-terminated names.
        let rc = unsafe { RegGetValueW(root(hive), k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut len) };
        if rc != ERROR_SUCCESS || len == 0 {
            return None;
        }
        let mut buf = vec![0u16; (len as usize).div_ceil(2)];
        // SAFETY: `buf` holds `len` bytes, as RegGetValueW was told.
        let rc = unsafe {
            RegGetValueW(root(hive), k.as_ptr(), v.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
        };
        (rc == ERROR_SUCCESS).then(|| super::string_from_wide(&buf))
    }

    /// A `REG_DWORD` value.
    pub fn read_dword(hive: Hive, key: &str, value: &str) -> Option<u32> {
        let (k, v) = (wide(key), wide(value));
        let mut bytes = [0u8; 4];
        let mut len: u32 = 4;
        // SAFETY: a 4-byte buffer for a REG_DWORD.
        let rc = unsafe {
            RegGetValueW(root(hive), k.as_ptr(), v.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), bytes.as_mut_ptr().cast(), &mut len)
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        super::dword_from_bytes(&bytes[..len as usize])
    }

    /// Set a `REG_SZ` value, creating the key if needed. Only ever under `HKEY_CURRENT_USER`.
    pub fn write_string(key: &str, value: &str, data: &str) -> std::io::Result<()> {
        let (k, v, d) = (wide(key), wide(value), wide(data));
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated names; `h` receives the opened key, closed below.
        let rc = unsafe {
            RegCreateKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, std::ptr::null(), &mut h, std::ptr::null_mut())
        };
        if rc != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        // SAFETY: `d` is the UTF-16 data including its NUL, as REG_SZ wants.
        let rc = unsafe { RegSetValueExW(h, v.as_ptr(), 0, REG_SZ, d.as_ptr().cast(), (d.len() * 2) as u32) };
        // SAFETY: `h` was opened above.
        unsafe { RegCloseKey(h) };
        if rc != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }

    /// Remove a value under `HKEY_CURRENT_USER` (a missing one is fine).
    pub fn delete_value(key: &str, value: &str) -> std::io::Result<()> {
        let (k, v) = (wide(key), wide(value));
        // SAFETY: valid NUL-terminated names.
        let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), v.as_ptr()) };
        const NOT_FOUND: u32 = windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        if rc != ERROR_SUCCESS && rc != NOT_FOUND {
            return Err(std::io::Error::from_raw_os_error(rc as i32));
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use sys::{delete_value, read_dword, read_string, write_string};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_registry_strings_and_dwords() {
        let w: Vec<u16> = "c:/program files (x86)/steam\0junk".encode_utf16().collect();
        assert_eq!(string_from_wide(&w), "c:/program files (x86)/steam");
        let unterminated: Vec<u16> = "abc".encode_utf16().collect();
        assert_eq!(string_from_wide(&unterminated), "abc");
        assert_eq!(string_from_wide(&[]), "");
        assert_eq!(dword_from_bytes(&[0x2a, 0x1b, 0, 0]), Some(0x1b2a));
        assert_eq!(dword_from_bytes(&[1, 2]), None);
    }

    #[test]
    fn steam_folder_from_registry_values() {
        let p = |s: &str| Some(std::path::PathBuf::from(s));
        assert_eq!(steam_dir_from(Some("c:/program files (x86)/steam"), None), p("c:\\program files (x86)\\steam"));
        assert_eq!(steam_dir_from(Some(""), Some("C:\\Program Files (x86)\\Steam")), p("C:\\Program Files (x86)\\Steam"));
        assert_eq!(steam_dir_from(None, Some("D:\\Steam")), p("D:\\Steam"));
        assert_eq!(steam_dir_from(None, None), None);
    }

    #[cfg(windows)]
    #[test]
    fn round_trip_under_current_user() {
        let key = "Software\\grapple-test";
        let name = format!("t{}", std::process::id());
        write_string(key, &name, "C:\\Games\\grapple ü").unwrap();
        assert_eq!(read_string(Hive::CurrentUser, key, &name).as_deref(), Some("C:\\Games\\grapple ü"));
        delete_value(key, &name).unwrap();
        assert_eq!(read_string(Hive::CurrentUser, key, &name), None);
        delete_value(key, &name).unwrap();
    }
}
