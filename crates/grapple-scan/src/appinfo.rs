//! Reader for Steam's binary `appcache/appinfo.vdf`.
//!
//! Layout (little endian):
//! ```text
//! header   u32 magic (0x07564427 / 28 / 29), u32 universe,
//!          [v29 only] i64 offset of the string table
//! entries  u32 appid (0 = end), u32 size, then `size` bytes:
//!            u32 infoState, u32 lastUpdated, u64 token, [20]u8 sha1, u32 changeNumber,
//!            [v28+] [20]u8 binary sha1, then a binary KeyValues tree
//! strtab   [v29] u32 count, then `count` NUL-terminated strings
//! ```
//! In the KeyValues tree each node is `u8 type`, a key (v29: `u32` index into the string table, older:
//! NUL-terminated), then a value. Types: 0 section, 1 string, 2 i32, 3 f32, 4 pointer, 5 wide string,
//! 6 colour, 7 u64, 10 i64; 8 ends a section.
//!
//! Only `appinfo.common.{name,type,clienticon}` is extracted, and only for the apps the caller asks for;
//! every other entry is skipped by its size without being parsed. v29 is verified against real files;
//! v27/v28 follow the same layout from the public format description and are covered by synthetic tests only.

use std::collections::HashMap;

const MAGIC_V27: u32 = 0x0756_4427;
const MAGIC_V28: u32 = 0x0756_4428;
const MAGIC_V29: u32 = 0x0756_4429;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppMeta {
    pub name: Option<String>,
    /// Lowercased `common.type`: `game`, `tool`, `application`, `dlc`, ...
    pub kind: String,
    /// `common.clienticon`: the file name stem of the 32px icon in the library cache.
    pub client_icon: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AppInfoError {
    #[error("not an appinfo.vdf file (magic {0:#010x})")]
    BadMagic(u32),
    #[error("appinfo.vdf is truncated or corrupt")]
    Truncated,
}

/// Parse the entries whose appid satisfies `wanted`. Entries that fail to parse are skipped.
pub fn parse(data: &[u8], wanted: impl Fn(u32) -> bool) -> Result<HashMap<u32, AppMeta>, AppInfoError> {
    let magic = u32_at(data, 0).ok_or(AppInfoError::Truncated)?;
    if !matches!(magic, MAGIC_V27 | MAGIC_V28 | MAGIC_V29) {
        return Err(AppInfoError::BadMagic(magic));
    }
    let mut strtab: Vec<&[u8]> = Vec::new();
    let mut off = 8;
    let end_of_entries;
    if magic == MAGIC_V29 {
        let so = i64_at(data, 8).ok_or(AppInfoError::Truncated)?;
        let so = usize::try_from(so).map_err(|_| AppInfoError::Truncated)?;
        let count = u32_at(data, so).ok_or(AppInfoError::Truncated)? as usize;
        let mut p = so + 4;
        strtab.reserve(count.min(1 << 20));
        for _ in 0..count {
            let rest = data.get(p..).ok_or(AppInfoError::Truncated)?;
            let n = rest.iter().position(|&b| b == 0).ok_or(AppInfoError::Truncated)?;
            strtab.push(&rest[..n]);
            p += n + 1;
        }
        off = 16;
        end_of_entries = so;
    } else {
        end_of_entries = data.len();
    }
    // Fixed bytes before the KeyValues tree.
    let prefix = if magic == MAGIC_V27 { 40 } else { 60 };

    let mut out = HashMap::new();
    while off + 8 <= end_of_entries {
        let appid = u32_at(data, off).ok_or(AppInfoError::Truncated)?;
        if appid == 0 {
            break;
        }
        let size = u32_at(data, off + 4).ok_or(AppInfoError::Truncated)? as usize;
        let body = off + 8;
        let end = body.checked_add(size).filter(|&e| e <= data.len()).ok_or(AppInfoError::Truncated)?;
        if wanted(appid) && size >= prefix {
            let mut r = Reader { d: &data[body + prefix..end], i: 0, strtab: &strtab, inline_keys: magic != MAGIC_V29 };
            if let Some(meta) = r.app() {
                out.insert(appid, meta);
            }
        }
        off = end;
    }
    Ok(out)
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn i64_at(d: &[u8], o: usize) -> Option<i64> {
    d.get(o..o + 8).map(|b| i64::from_le_bytes(b.try_into().unwrap()))
}

struct Reader<'a> {
    d: &'a [u8],
    i: usize,
    strtab: &'a [&'a [u8]],
    inline_keys: bool,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.d.get(self.i)?;
        self.i += 1;
        Some(b)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.i.checked_add(n).filter(|&e| e <= self.d.len())?;
        self.i = end;
        Some(())
    }

    fn cstr(&mut self) -> Option<&'a [u8]> {
        let rest = self.d.get(self.i..)?;
        let n = rest.iter().position(|&b| b == 0)?;
        self.i += n + 1;
        Some(&rest[..n])
    }

    fn key(&mut self) -> Option<&'a [u8]> {
        if self.inline_keys {
            self.cstr()
        } else {
            let idx = u32_at(self.d, self.i)? as usize;
            self.i += 4;
            self.strtab.get(idx).copied()
        }
    }

    /// Skip a value of type `t` (the key has been read).
    fn skip_value(&mut self, t: u8, depth: u32) -> Option<()> {
        match t {
            0 => self.skip_section(depth + 1),
            1 => self.cstr().map(|_| ()),
            2 | 3 | 4 | 6 => self.skip(4),
            7 | 10 => self.skip(8),
            5 => {
                // UTF-16 string, NUL terminated by a zero code unit.
                loop {
                    let pair = self.d.get(self.i..self.i + 2)?;
                    self.i += 2;
                    if pair == [0, 0] {
                        return Some(());
                    }
                }
            }
            _ => None,
        }
    }

    fn skip_section(&mut self, depth: u32) -> Option<()> {
        if depth > 64 {
            return None;
        }
        loop {
            let t = self.byte()?;
            if t == 8 {
                return Some(());
            }
            self.key()?;
            self.skip_value(t, depth)?;
        }
    }

    /// Root object: usually `appinfo { common { .. } .. }`, tolerate `common` directly at the root.
    fn app(&mut self) -> Option<AppMeta> {
        let mut meta = None;
        loop {
            let t = self.byte()?;
            if t == 8 {
                return meta;
            }
            let key = self.key()?;
            match (t, key) {
                (0, b"appinfo") => {
                    if let Some(m) = self.app() {
                        meta = Some(m);
                    }
                }
                (0, b"common") => meta = Some(self.common()?),
                _ => self.skip_value(t, 0)?,
            }
        }
    }

    fn common(&mut self) -> Option<AppMeta> {
        let mut m = AppMeta::default();
        loop {
            let t = self.byte()?;
            if t == 8 {
                return Some(m);
            }
            let key = self.key()?;
            if t == 1 {
                let v = self.cstr()?;
                match key {
                    b"name" => m.name = Some(String::from_utf8_lossy(v).into_owned()),
                    b"type" => m.kind = String::from_utf8_lossy(v).to_ascii_lowercase(),
                    b"clienticon" => m.client_icon = Some(String::from_utf8_lossy(v).into_owned()),
                    _ => {}
                }
            } else {
                self.skip_value(t, 0)?;
            }
        }
    }
}
