//! Epic's binary manifest: the list of files of one build, the chunks they are made of, and where in each
//! chunk each file's bytes are. Written from the format description in `docs/DOWNLOADS_PLAN.md` (verified on a
//! real manifest on 2026-10-20). All integers are little-endian.
//!
//! The parser checks every length against the bytes that are left, so a broken or hostile file fails with
//! [`Error::Format`] instead of allocating or reading out of bounds.

use crate::error::{Error, Result};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::fmt;
use std::io::Read;

pub const MANIFEST_MAGIC: u32 = 0x44BE_C00C;
/// The header size of current files (the parser follows the size the file states).
pub const HEADER_SIZE: u32 = 41;
/// The body may not claim more than this when uncompressed (a real 20 937-file manifest is a few MiB).
const MAX_BODY: u32 = 512 * 1024 * 1024;

/// A chunk id: four u32. Its text form is the four as `%08X`, concatenated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Guid(pub [u32; 4]);

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08X}{:08X}{:08X}{:08X}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

impl Guid {
    /// The text form back into a guid (the journal stores guids as text).
    pub fn parse(s: &str) -> Option<Guid> {
        if s.len() != 32 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut g = [0u32; 4];
        for (i, v) in g.iter_mut().enumerate() {
            *v = u32::from_str_radix(&s[i * 8..i * 8 + 8], 16).ok()?;
        }
        Some(Guid(g))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    pub data_version: u8,
    pub feature_level: u32,
    pub is_file_data: bool,
    pub app_id: u32,
    pub app_name: String,
    pub build_version: String,
    pub launch_exe: String,
    pub launch_command: String,
    pub prereq_ids: Vec<String>,
    pub prereq_name: String,
    pub prereq_path: String,
    pub prereq_args: String,
    pub build_id: String,
    pub uninstall_action_path: String,
    pub uninstall_action_args: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkInfo {
    pub guid: Guid,
    pub rolling_hash: u64,
    pub sha1: [u8; 20],
    pub group: u8,
    /// Uncompressed size (1 MiB in practice).
    pub window_size: u32,
    /// Size of the `.chunk` file on the CDN: what downloading it costs.
    pub file_size: i64,
}

impl ChunkInfo {
    /// `<chunk_dir>/<group:02>/<rolling_hash:016X>_<guid>.chunk`, relative to a base URL.
    pub fn path(&self, chunk_dir: &str) -> String {
        format!("{chunk_dir}/{:02}/{:016X}_{}.chunk", self.group, self.rolling_hash, self.guid)
    }
    pub fn download_size(&self) -> u64 {
        self.file_size.max(0) as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPart {
    pub guid: Guid,
    /// Offset into the uncompressed chunk.
    pub offset: u32,
    pub size: u32,
}

pub mod flags {
    pub const READ_ONLY: u8 = 0x1;
    pub const COMPRESSED: u8 = 0x2;
    pub const UNIX_EXECUTABLE: u8 = 0x4;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileEntry {
    /// Relative path with `/` separators (as stored; check it with [`crate::fsutil::safe_rel`] before use).
    pub name: String,
    pub symlink_target: String,
    pub sha1: [u8; 20],
    pub flags: u8,
    pub install_tags: Vec<String>,
    pub parts: Vec<ChunkPart>,
    pub md5: Option<[u8; 16]>,
    pub mime_type: String,
    pub sha256: Option<[u8; 32]>,
}

impl FileEntry {
    /// Sum of its parts.
    pub fn size(&self) -> u64 {
        self.parts.iter().map(|p| p.size as u64).sum()
    }
    pub fn is_symlink(&self) -> bool {
        !self.symlink_target.is_empty()
    }
    pub fn executable(&self) -> bool {
        self.flags & flags::UNIX_EXECUTABLE != 0
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    /// The header's feature version (21 for current files); picks the chunk folder.
    pub version: u32,
    pub meta: Meta,
    pub chunks: Vec<ChunkInfo>,
    pub files: Vec<FileEntry>,
    pub custom_fields: Vec<(String, String)>,
}

impl Manifest {
    /// The chunk folder on the CDN for this manifest version.
    pub fn chunk_dir(&self) -> &'static str {
        chunk_dir(self.version)
    }

    pub fn chunk_index(&self) -> HashMap<Guid, usize> {
        self.chunks.iter().enumerate().map(|(i, c)| (c.guid, i)).collect()
    }

    /// Files v1 installs: every file with no install tags plus every tagged file (no selective install yet).
    pub fn install_files(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.iter()
    }

    /// Sum of all file sizes.
    pub fn install_size(&self) -> u64 {
        self.install_files().map(FileEntry::size).sum()
    }

    pub fn custom(&self, key: &str) -> Option<&str> {
        self.custom_fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

pub fn chunk_dir(version: u32) -> &'static str {
    match version {
        v if v >= 15 => "ChunksV4",
        v if v >= 6 => "ChunksV3",
        v if v >= 3 => "ChunksV2",
        _ => "Chunks",
    }
}

pub fn sha1(bytes: &[u8]) -> [u8; 20] {
    Sha1::digest(bytes).into()
}

pub fn hex(bytes: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

/// A bounds-checked little-endian reader.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn bad(what: impl Into<String>) -> Error {
    Error::Format(what.into())
}

impl<'a> Reader<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }
    pub(crate) fn seek(&mut self, to: usize) -> Result<()> {
        if to > self.buf.len() {
            return Err(bad("section runs past the end"));
        }
        self.pos = to;
        Ok(())
    }
    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if n > self.remaining() {
            return Err(bad("unexpected end of data"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }
    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub(crate) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }
    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    pub(crate) fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    pub(crate) fn guid(&mut self) -> Result<Guid> {
        Ok(Guid([self.u32()?, self.u32()?, self.u32()?, self.u32()?]))
    }

    /// FString: `i32 n`; 0 → ""; n > 0 → n bytes (UTF-8/ASCII) with a trailing NUL; n < 0 → -n UTF-16LE code
    /// units with a trailing NUL.
    pub(crate) fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let raw = self.bytes(n as usize)?;
            let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
            return String::from_utf8(raw.to_vec()).map_err(|_| bad("string is not UTF-8"));
        }
        let units = (n as i64).unsigned_abs() as usize;
        let raw = self.bytes(units.checked_mul(2).ok_or_else(|| bad("string too long"))?)?;
        let mut v: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        if v.last() == Some(&0) {
            v.pop();
        }
        String::from_utf16(&v).map_err(|_| bad("string is not UTF-16"))
    }

    /// A count of items that each take at least `min_each` bytes: fails when they cannot fit.
    pub(crate) fn count(&mut self, min_each: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_each.max(1)) > self.remaining() {
            return Err(bad("count larger than the data"));
        }
        Ok(n)
    }
}

/// The header of a manifest file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub header_size: u32,
    pub size_uncompressed: u32,
    pub size_compressed: u32,
    pub sha1: [u8; 20],
    pub stored_as: u8,
    pub version: u32,
}

/// Parse the raw bytes of a `.manifest` file.
pub fn parse(data: &[u8]) -> Result<Manifest> {
    if data.first() == Some(&b'{') {
        return Err(Error::OldManifest);
    }
    let mut r = Reader::new(data);
    if r.u32()? != MANIFEST_MAGIC {
        return Err(bad("not a manifest (magic)"));
    }
    let h = Header {
        header_size: r.u32()?,
        size_uncompressed: r.u32()?,
        size_compressed: r.u32()?,
        sha1: r.array()?,
        stored_as: r.u8()?,
        version: 0,
    };
    // 37 bytes so far; the version follows in current files.
    let version = if h.header_size >= HEADER_SIZE { r.u32()? } else { 0 };
    let h = Header { version, ..h };
    if (h.header_size as usize) < 37 || h.header_size as usize > data.len() {
        return Err(bad("bad header size"));
    }
    if h.size_uncompressed > MAX_BODY {
        return Err(bad("manifest too large"));
    }
    let start = h.header_size as usize;
    let body: Vec<u8> = if h.stored_as & 1 != 0 {
        let end = start.checked_add(h.size_compressed as usize).filter(|e| *e <= data.len()).ok_or_else(|| bad("compressed body runs past the end"))?;
        let mut out = Vec::with_capacity(h.size_uncompressed as usize);
        flate2::read::ZlibDecoder::new(&data[start..end])
            .take(h.size_uncompressed as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| bad(format!("zlib: {e}")))?;
        out
    } else {
        let end = start.checked_add(h.size_uncompressed as usize).filter(|e| *e <= data.len()).ok_or_else(|| bad("body runs past the end"))?;
        data[start..end].to_vec()
    };
    if body.len() != h.size_uncompressed as usize {
        return Err(bad("body size does not match the header"));
    }
    if sha1(&body) != h.sha1 {
        return Err(bad("body hash does not match the header"));
    }
    parse_body(&body, h.version)
}

fn parse_body(body: &[u8], version: u32) -> Result<Manifest> {
    let mut r = Reader::new(body);
    let meta = section(&mut r, read_meta)?;
    let chunks = section(&mut r, read_chunks)?;
    let files = section(&mut r, read_files)?;
    // Some old files end after the file list.
    let custom_fields = if r.remaining() >= 4 { section(&mut r, read_custom)? } else { Vec::new() };
    let m = Manifest { version, meta, chunks, files, custom_fields };
    check_parts(&m)?;
    Ok(m)
}

/// Run `f` on a section: `u32 size` from the section's first byte, then seek to start + size.
fn section<T>(r: &mut Reader<'_>, f: impl FnOnce(&mut Reader<'_>) -> Result<T>) -> Result<T> {
    let start = r.pos();
    let size = r.u32()? as usize;
    let end = start.checked_add(size).ok_or_else(|| bad("section size"))?;
    if size < 4 || end > start + 4 + r.remaining() {
        return Err(bad("section size"));
    }
    let v = f(r)?;
    if r.pos() > end {
        return Err(bad("section is longer than its size"));
    }
    r.seek(end)?;
    Ok(v)
}

fn read_meta(r: &mut Reader<'_>) -> Result<Meta> {
    let data_version = r.u8()?;
    let mut m = Meta {
        data_version,
        feature_level: r.u32()?,
        is_file_data: r.u8()? != 0,
        app_id: r.u32()?,
        app_name: r.fstring()?,
        build_version: r.fstring()?,
        launch_exe: r.fstring()?,
        launch_command: r.fstring()?,
        ..Meta::default()
    };
    let n = r.count(4)?;
    for _ in 0..n {
        m.prereq_ids.push(r.fstring()?);
    }
    m.prereq_name = r.fstring()?;
    m.prereq_path = r.fstring()?;
    m.prereq_args = r.fstring()?;
    if data_version >= 1 {
        m.build_id = r.fstring()?;
    }
    if data_version >= 2 {
        m.uninstall_action_path = r.fstring()?;
        m.uninstall_action_args = r.fstring()?;
    }
    Ok(m)
}

fn read_chunks(r: &mut Reader<'_>) -> Result<Vec<ChunkInfo>> {
    let _version = r.u8()?;
    // 16 guid + 8 hash + 20 sha + 1 group + 4 window + 8 size
    let n = r.count(57)?;
    let guids = (0..n).map(|_| r.guid()).collect::<Result<Vec<_>>>()?;
    let hashes = (0..n).map(|_| r.u64()).collect::<Result<Vec<_>>>()?;
    let shas = (0..n).map(|_| r.array::<20>()).collect::<Result<Vec<_>>>()?;
    let groups = (0..n).map(|_| r.u8()).collect::<Result<Vec<_>>>()?;
    let windows = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
    let sizes = (0..n).map(|_| r.i64()).collect::<Result<Vec<_>>>()?;
    Ok((0..n)
        .map(|i| ChunkInfo { guid: guids[i], rolling_hash: hashes[i], sha1: shas[i], group: groups[i], window_size: windows[i], file_size: sizes[i] })
        .collect())
}

fn read_files(r: &mut Reader<'_>) -> Result<Vec<FileEntry>> {
    let version = r.u8()?;
    // at least: 4 name + 4 symlink + 20 sha + 1 flags + 4 tag count + 4 part count
    let m = r.count(37)?;
    let mut files: Vec<FileEntry> = vec![FileEntry::default(); m];
    for f in &mut files {
        f.name = r.fstring()?.replace('\\', "/");
    }
    for f in &mut files {
        f.symlink_target = r.fstring()?;
    }
    for f in &mut files {
        f.sha1 = r.array()?;
    }
    for f in &mut files {
        f.flags = r.u8()?;
    }
    for f in &mut files {
        let k = r.count(4)?;
        for _ in 0..k {
            f.install_tags.push(r.fstring()?);
        }
    }
    for f in &mut files {
        let parts = r.count(28)?;
        f.parts.reserve(parts);
        for _ in 0..parts {
            let start = r.pos();
            let part_size = r.u32()? as usize;
            if part_size < 28 {
                return Err(bad("chunk part size"));
            }
            let p = ChunkPart { guid: r.guid()?, offset: r.u32()?, size: r.u32()? };
            r.seek(start.checked_add(part_size).ok_or_else(|| bad("chunk part size"))?)?;
            f.parts.push(p);
        }
    }
    if version >= 1 {
        for f in &mut files {
            if r.u32()? != 0 {
                f.md5 = Some(r.array()?);
            }
        }
        for f in &mut files {
            f.mime_type = r.fstring()?;
        }
    }
    if version >= 2 {
        for f in &mut files {
            f.sha256 = Some(r.array()?);
        }
    }
    Ok(files)
}

fn read_custom(r: &mut Reader<'_>) -> Result<Vec<(String, String)>> {
    let _version = r.u8()?;
    let n = r.count(8)?;
    let keys = (0..n).map(|_| r.fstring()).collect::<Result<Vec<_>>>()?;
    let values = (0..n).map(|_| r.fstring()).collect::<Result<Vec<_>>>()?;
    Ok(keys.into_iter().zip(values).collect())
}

/// Every part must name a chunk of the list and stay inside its window.
fn check_parts(m: &Manifest) -> Result<()> {
    let idx = m.chunk_index();
    for f in &m.files {
        for p in &f.parts {
            let c = idx.get(&p.guid).map(|&i| &m.chunks[i]).ok_or_else(|| bad(format!("{} uses an unknown chunk", f.name)))?;
            if p.offset as u64 + p.size as u64 > c.window_size as u64 {
                return Err(bad(format!("{} reads past the end of a chunk", f.name)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_text_round_trip() {
        let g = Guid([0x2D02FB2D, 0x40914F7B, 0x77D1CE91, 0x7FD0F547]);
        assert_eq!(g.to_string(), "2D02FB2D40914F7B77D1CE917FD0F547");
        assert_eq!(Guid::parse(&g.to_string()), Some(g));
        assert_eq!(Guid::parse("xyz"), None);
    }

    #[test]
    fn chunk_path_like_the_real_one() {
        let c = ChunkInfo {
            guid: Guid([0x2D02FB2D, 0x40914F7B, 0x77D1CE91, 0x7FD0F547]),
            rolling_hash: 0x8D4989EDD550784B,
            sha1: [0; 20],
            group: 96,
            window_size: 1 << 20,
            file_size: 1,
        };
        assert_eq!(c.path(chunk_dir(21)), "ChunksV4/96/8D4989EDD550784B_2D02FB2D40914F7B77D1CE917FD0F547.chunk");
        assert_eq!(chunk_dir(14), "ChunksV3");
        assert_eq!(chunk_dir(3), "ChunksV2");
        assert_eq!(chunk_dir(2), "Chunks");
    }

    fn fs_bytes(n: i32, payload: &[u8]) -> Vec<u8> {
        let mut v = n.to_le_bytes().to_vec();
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn fstring_edge_cases() {
        assert_eq!(Reader::new(&fs_bytes(0, &[])).fstring().unwrap(), "");
        assert_eq!(Reader::new(&fs_bytes(4, b"abc\0")).fstring().unwrap(), "abc");
        // UTF-16: "é✓" + NUL = 3 code units
        let mut u = Vec::new();
        for c in "é✓\0".encode_utf16() {
            u.extend_from_slice(&c.to_le_bytes());
        }
        assert_eq!(Reader::new(&fs_bytes(-3, &u)).fstring().unwrap(), "é✓");
        // claims more than there is
        assert!(Reader::new(&fs_bytes(100, b"ab")).fstring().is_err());
        assert!(Reader::new(&fs_bytes(i32::MIN, b"ab")).fstring().is_err());
        // a single NUL is an empty string
        assert_eq!(Reader::new(&fs_bytes(1, b"\0")).fstring().unwrap(), "");
    }

    #[test]
    fn json_manifest_is_refused() {
        assert!(matches!(parse(b"{\"ManifestFileVersion\":\"x\"}"), Err(Error::OldManifest)));
        assert!(matches!(parse(b"\x00\x01"), Err(Error::Format(_))));
    }
}
