//! TEST ONLY (feature `testkit`): builds binary manifests and chunk files from a small in-memory "game", in the
//! format `manifest.rs` and `chunk.rs` read. Never part of a grapple build.

use crate::manifest::{self, ChunkInfo, ChunkPart, FileEntry, Guid, Manifest, Meta, sha1};
use std::collections::BTreeMap;
use std::io::Write;

#[derive(Debug, Clone, Default)]
pub struct TestFile {
    pub name: String,
    pub data: Vec<u8>,
    pub flags: u8,
    pub tags: Vec<String>,
    pub symlink: Option<String>,
}

impl TestFile {
    pub fn new(name: &str, data: impl Into<Vec<u8>>) -> TestFile {
        TestFile { name: name.into(), data: data.into(), ..TestFile::default() }
    }
    pub fn exec(mut self) -> TestFile {
        self.flags |= manifest::flags::UNIX_EXECUTABLE;
        self
    }
}

/// Bytes that differ per (seed, length) and do not compress to nothing.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct GameSpec {
    pub app_name: String,
    pub build_version: String,
    pub launch_exe: String,
    pub launch_command: String,
    pub prereq_ids: Vec<String>,
    pub prereq_path: String,
    pub prereq_args: String,
    pub uninstall_path: String,
    pub uninstall_args: String,
    /// Header feature version (picks the chunk folder).
    pub version: u32,
    /// Uncompressed chunk size.
    pub window: u32,
    /// Seed for chunk ids, so two builds of one game have different chunks.
    pub seed: u32,
    pub compress_manifest: bool,
    pub file_list_version: u8,
    pub files: Vec<TestFile>,
}

impl Default for GameSpec {
    fn default() -> Self {
        GameSpec {
            app_name: "TestGame".into(),
            build_version: "1.0.0-CL-1".into(),
            launch_exe: "Binaries/Win64/Game.exe".into(),
            launch_command: "-nosplash".into(),
            prereq_ids: vec!["EAC_Service".into()],
            prereq_path: "EasyAntiCheat/Setup.exe".into(),
            prereq_args: "install abc".into(),
            uninstall_path: "EasyAntiCheat/Setup.exe".into(),
            uninstall_args: "uninstall abc".into(),
            version: 21,
            window: 64 * 1024,
            seed: 1,
            compress_manifest: true,
            file_list_version: 2,
            files: Vec::new(),
        }
    }
}

pub struct Built {
    pub manifest_bytes: Vec<u8>,
    pub manifest: Manifest,
    /// CDN path (`ChunksV4/..`) → chunk file bytes.
    pub chunks: BTreeMap<String, Vec<u8>>,
}

impl Built {
    pub fn total_download(&self) -> u64 {
        self.manifest.chunks.iter().map(ChunkInfo::download_size).sum()
    }
}

/// Lay the files' bytes end to end and cut them into windows: small files share a chunk, big ones span several.
/// Even chunks are zlib-compressed, odd ones stored raw.
pub fn build(spec: &GameSpec) -> Built {
    let w = spec.window as usize;
    let mut windows: Vec<Vec<u8>> = vec![Vec::new()];
    let mut files = Vec::new();
    let mut ids = Vec::new();
    let guid = |i: usize| Guid([spec.seed, 0xA000_0000 | i as u32, 0x1234_5678 ^ i as u32, 0xC0FF_EE00 + spec.seed]);
    for f in &spec.files {
        let mut parts = Vec::new();
        if f.symlink.is_none() {
            let mut left = &f.data[..];
            while !left.is_empty() {
                if windows.last().unwrap().len() == w {
                    windows.push(Vec::new());
                }
                let i = windows.len() - 1;
                let cur = windows.last_mut().unwrap();
                let off = cur.len();
                let take = (w - off).min(left.len());
                cur.extend_from_slice(&left[..take]);
                parts.push(ChunkPart { guid: guid(i), offset: off as u32, size: take as u32 });
                left = &left[take..];
            }
        }
        files.push(FileEntry {
            name: f.name.clone(),
            symlink_target: f.symlink.clone().unwrap_or_default(),
            sha1: sha1(&f.data),
            flags: f.flags,
            install_tags: f.tags.clone(),
            parts,
            md5: (spec.file_list_version >= 1).then_some([7u8; 16]),
            mime_type: if spec.file_list_version >= 1 { "application/octet-stream".into() } else { String::new() },
            sha256: (spec.file_list_version >= 2).then_some([9u8; 32]),
        });
    }
    if windows.last().is_some_and(Vec::is_empty) {
        windows.pop();
    }
    let mut chunks = Vec::new();
    let mut chunk_files = BTreeMap::new();
    for (i, mut data) in windows.into_iter().enumerate() {
        data.resize(w, 0);
        let g = guid(i);
        ids.push(g);
        let compress = i % 2 == 0;
        let file = chunk_file(g, 0x8D49_89ED_D550_7800 + i as u64 + spec.seed as u64 * 1000, &data, compress);
        let info = ChunkInfo {
            guid: g,
            rolling_hash: 0x8D49_89ED_D550_7800 + i as u64 + spec.seed as u64 * 1000,
            sha1: sha1(&data),
            group: (i % 100) as u8,
            window_size: spec.window,
            file_size: file.len() as i64,
        };
        chunk_files.insert(info.path(manifest::chunk_dir(spec.version)), file);
        chunks.push(info);
    }
    let m = Manifest {
        version: spec.version,
        meta: Meta {
            data_version: 2,
            feature_level: spec.version,
            is_file_data: false,
            app_id: 0,
            app_name: spec.app_name.clone(),
            build_version: spec.build_version.clone(),
            launch_exe: spec.launch_exe.clone(),
            launch_command: spec.launch_command.clone(),
            prereq_ids: spec.prereq_ids.clone(),
            prereq_name: String::new(),
            prereq_path: spec.prereq_path.clone(),
            prereq_args: spec.prereq_args.clone(),
            build_id: "build-id".into(),
            uninstall_action_path: spec.uninstall_path.clone(),
            uninstall_action_args: spec.uninstall_args.clone(),
        },
        chunks,
        files,
        custom_fields: vec![("CloudSaveFolder".into(), "{AppData}/x".into())],
    };
    Built { manifest_bytes: write_manifest(&m, spec.compress_manifest, spec.file_list_version), manifest: m, chunks: chunk_files }
}

fn fstring(out: &mut Vec<u8>, s: &str) {
    if s.is_empty() {
        out.extend_from_slice(&0i32.to_le_bytes());
    } else if s.is_ascii() {
        out.extend_from_slice(&(s.len() as i32 + 1).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
        out.push(0);
    } else {
        let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
        out.extend_from_slice(&(-(units.len() as i32)).to_le_bytes());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
}

fn section(out: &mut Vec<u8>, body: Vec<u8>) {
    out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    out.extend_from_slice(&body);
}

fn guid(out: &mut Vec<u8>, g: Guid) {
    for v in g.0 {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

pub fn write_manifest(m: &Manifest, compress: bool, file_list_version: u8) -> Vec<u8> {
    let mut body = Vec::new();
    // meta
    let mut s = Vec::new();
    let meta = &m.meta;
    s.push(meta.data_version);
    s.extend_from_slice(&meta.feature_level.to_le_bytes());
    s.push(meta.is_file_data as u8);
    s.extend_from_slice(&meta.app_id.to_le_bytes());
    for t in [&meta.app_name, &meta.build_version, &meta.launch_exe, &meta.launch_command] {
        fstring(&mut s, t);
    }
    s.extend_from_slice(&(meta.prereq_ids.len() as u32).to_le_bytes());
    for p in &meta.prereq_ids {
        fstring(&mut s, p);
    }
    for t in [&meta.prereq_name, &meta.prereq_path, &meta.prereq_args] {
        fstring(&mut s, t);
    }
    if meta.data_version >= 1 {
        fstring(&mut s, &meta.build_id);
    }
    if meta.data_version >= 2 {
        fstring(&mut s, &meta.uninstall_action_path);
        fstring(&mut s, &meta.uninstall_action_args);
    }
    section(&mut body, s);
    // chunks
    let mut s = vec![0u8];
    s.extend_from_slice(&(m.chunks.len() as u32).to_le_bytes());
    m.chunks.iter().for_each(|c| guid(&mut s, c.guid));
    m.chunks.iter().for_each(|c| s.extend_from_slice(&c.rolling_hash.to_le_bytes()));
    m.chunks.iter().for_each(|c| s.extend_from_slice(&c.sha1));
    m.chunks.iter().for_each(|c| s.push(c.group));
    m.chunks.iter().for_each(|c| s.extend_from_slice(&c.window_size.to_le_bytes()));
    m.chunks.iter().for_each(|c| s.extend_from_slice(&c.file_size.to_le_bytes()));
    section(&mut body, s);
    // files
    let mut s = vec![file_list_version];
    s.extend_from_slice(&(m.files.len() as u32).to_le_bytes());
    m.files.iter().for_each(|f| fstring(&mut s, &f.name));
    m.files.iter().for_each(|f| fstring(&mut s, &f.symlink_target));
    m.files.iter().for_each(|f| s.extend_from_slice(&f.sha1));
    m.files.iter().for_each(|f| s.push(f.flags));
    for f in &m.files {
        s.extend_from_slice(&(f.install_tags.len() as u32).to_le_bytes());
        f.install_tags.iter().for_each(|t| fstring(&mut s, t));
    }
    for f in &m.files {
        s.extend_from_slice(&(f.parts.len() as u32).to_le_bytes());
        for p in &f.parts {
            s.extend_from_slice(&28u32.to_le_bytes());
            guid(&mut s, p.guid);
            s.extend_from_slice(&p.offset.to_le_bytes());
            s.extend_from_slice(&p.size.to_le_bytes());
        }
    }
    if file_list_version >= 1 {
        for f in &m.files {
            match f.md5 {
                Some(md5) => {
                    s.extend_from_slice(&1u32.to_le_bytes());
                    s.extend_from_slice(&md5);
                }
                None => s.extend_from_slice(&0u32.to_le_bytes()),
            }
        }
        m.files.iter().for_each(|f| fstring(&mut s, &f.mime_type));
    }
    if file_list_version >= 2 {
        m.files.iter().for_each(|f| s.extend_from_slice(&f.sha256.unwrap_or([0; 32])));
    }
    section(&mut body, s);
    // custom fields
    let mut s = vec![0u8];
    s.extend_from_slice(&(m.custom_fields.len() as u32).to_le_bytes());
    m.custom_fields.iter().for_each(|(k, _)| fstring(&mut s, k));
    m.custom_fields.iter().for_each(|(_, v)| fstring(&mut s, v));
    section(&mut body, s);

    let stored = if compress { zlib(&body) } else { body.clone() };
    let mut out = Vec::new();
    out.extend_from_slice(&manifest::MANIFEST_MAGIC.to_le_bytes());
    out.extend_from_slice(&manifest::HEADER_SIZE.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&(stored.len() as u32).to_le_bytes());
    out.extend_from_slice(&sha1(&body));
    out.push(compress as u8);
    out.extend_from_slice(&m.version.to_le_bytes());
    out.extend_from_slice(&stored);
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// A chunk file, header version 3 (66 bytes), hash type 2 (sha1).
pub fn chunk_file(g: Guid, rolling_hash: u64, window: &[u8], compress: bool) -> Vec<u8> {
    let stored = if compress { zlib(window) } else { window.to_vec() };
    let mut out = Vec::new();
    out.extend_from_slice(&crate::chunk::CHUNK_MAGIC.to_le_bytes());
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&66u32.to_le_bytes());
    out.extend_from_slice(&(stored.len() as u32).to_le_bytes());
    guid(&mut out, g);
    out.extend_from_slice(&rolling_hash.to_le_bytes());
    out.push(compress as u8);
    out.extend_from_slice(&sha1(window));
    out.push(2);
    out.extend_from_slice(&(window.len() as u32).to_le_bytes());
    assert_eq!(out.len(), 66);
    out.extend_from_slice(&stored);
    out
}

/// The small game the tests install: nested folders, a UTF-16 file name, an empty file, a file spanning 3 chunks,
/// two small files sharing a chunk, an executable.
pub fn sample_files(window: usize, seed: u64) -> Vec<TestFile> {
    vec![
        TestFile::new("Binaries/Win64/Game.exe", noise(seed, window / 3)).exec(),
        TestFile::new("Content/Paks/pakchunk0.pak", noise(seed + 1, window * 2 + window / 2)),
        TestFile::new("Content/Movies/Ünïcødé ✓ intro.bk2", noise(seed + 2, 1000)),
        TestFile::new("Content/empty.txt", Vec::new()),
        TestFile::new("Engine/Config/base.ini", b"[Core]\nx=1\n".to_vec()),
        TestFile::new("readme.txt", noise(seed + 3, 300)),
    ]
}
