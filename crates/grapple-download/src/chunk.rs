//! A `.chunk` file from Epic's CDN: a header and up to one window (1 MiB) of data, usually zlib-compressed.
//! Written from the format in `docs/DOWNLOADS_PLAN.md` (verified on a real chunk: header version 3, 66 bytes).

use crate::error::{Error, Result};
use crate::manifest::{ChunkInfo, Guid, Reader, sha1};
use std::io::Read;

pub const CHUNK_MAGIC: u32 = 0xB1FE_3AA2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkHeader {
    pub header_version: u32,
    pub header_size: u32,
    pub compressed_size: u32,
    pub guid: Guid,
    pub rolling_hash: u64,
    pub stored_as: u8,
    pub sha1: Option<[u8; 20]>,
    pub hash_type: u8,
    pub uncompressed_size: Option<u32>,
}

fn bad(what: impl Into<String>) -> Error {
    Error::Format(what.into())
}

pub fn parse_header(data: &[u8]) -> Result<ChunkHeader> {
    let mut r = Reader::new(data);
    if r.u32()? != CHUNK_MAGIC {
        return Err(bad("not a chunk (magic)"));
    }
    let header_version = r.u32()?;
    let header_size = r.u32()?;
    let compressed_size = r.u32()?;
    let guid = r.guid()?;
    let rolling_hash = r.u64()?;
    let stored_as = r.u8()?;
    let (mut sha, mut hash_type, mut uncompressed_size) = (None, 0, None);
    if header_version >= 2 {
        sha = Some(r.array::<20>()?);
        hash_type = r.u8()?;
    }
    if header_version >= 3 {
        uncompressed_size = Some(r.u32()?);
    }
    if (header_size as usize) < r.pos() || header_size as usize > data.len() {
        return Err(bad("bad chunk header size"));
    }
    Ok(ChunkHeader { header_version, header_size, compressed_size, guid, rolling_hash, stored_as, sha1: sha, hash_type, uncompressed_size })
}

/// Unpack a downloaded chunk and check it against what the manifest says: the guid, the size of a window and the
/// SHA-1 of the data. Returns the uncompressed window.
pub fn decode(data: &[u8], want: &ChunkInfo) -> Result<Vec<u8>> {
    let h = parse_header(data)?;
    if h.guid != want.guid {
        return Err(bad("chunk has the wrong id"));
    }
    let start = h.header_size as usize;
    let end = start.checked_add(h.compressed_size as usize).filter(|e| *e <= data.len()).ok_or_else(|| bad("chunk data runs past the end"))?;
    let raw = &data[start..end];
    let window = want.window_size as usize;
    let out = if h.stored_as & 1 != 0 {
        let mut out = Vec::with_capacity(window);
        flate2::read::ZlibDecoder::new(raw).take(window as u64 + 1).read_to_end(&mut out).map_err(|e| bad(format!("zlib: {e}")))?;
        out
    } else {
        raw.to_vec()
    };
    if out.len() != window || h.uncompressed_size.is_some_and(|u| u as usize != window) {
        return Err(bad("chunk has the wrong size"));
    }
    let got = sha1(&out);
    // hash_type 2 (sha1) or 3 (both): the chunk carries its own hash too.
    if matches!(h.hash_type, 2 | 3) && h.sha1.is_some_and(|s| s != got) {
        return Err(bad("chunk hash does not match its header"));
    }
    if want.sha1 != [0; 20] && want.sha1 != got {
        return Err(bad("chunk hash does not match the manifest"));
    }
    Ok(out)
}
