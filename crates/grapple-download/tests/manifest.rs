//! Round trips through the test writer: every field of the manifest and chunk formats, the header and section
//! checks, and broken files.

use grapple_download::chunk;
use grapple_download::manifest::{self, Guid};
use grapple_download::testkit::{GameSpec, TestFile, build, chunk_file, noise, sample_files, write_manifest};
use grapple_download::Error;

fn spec() -> GameSpec {
    let mut s = GameSpec { window: 4096, ..GameSpec::default() };
    s.files = sample_files(4096, 7);
    s.files.push(TestFile { name: "tagged/dlc.pak".into(), data: noise(99, 50), tags: vec!["dlc".into(), "hd".into()], ..TestFile::default() });
    s.files.push(TestFile { name: "Binaries/link".into(), symlink: Some("Win64/Game.exe".into()), ..TestFile::default() });
    s
}

#[test]
fn round_trip_every_field() {
    for (compress, flv) in [(true, 2), (false, 2), (true, 1), (true, 0)] {
        let mut s = spec();
        s.compress_manifest = compress;
        s.file_list_version = flv;
        let b = build(&s);
        let m = manifest::parse(&b.manifest_bytes).unwrap();
        assert_eq!(m.version, 21);
        assert_eq!(m.meta, b.manifest.meta);
        assert_eq!(m.chunks, b.manifest.chunks);
        assert_eq!(m.custom_fields, b.manifest.custom_fields);
        assert_eq!(m.files.len(), b.manifest.files.len());
        for (a, e) in m.files.iter().zip(&b.manifest.files) {
            assert_eq!(a.name, e.name);
            assert_eq!(a.symlink_target, e.symlink_target);
            assert_eq!(a.sha1, e.sha1);
            assert_eq!(a.flags, e.flags);
            assert_eq!(a.install_tags, e.install_tags);
            assert_eq!(a.parts, e.parts);
            assert_eq!(a.md5, if flv >= 1 { e.md5 } else { None });
            assert_eq!(a.sha256, if flv >= 2 { e.sha256 } else { None });
        }
        assert_eq!(m.chunk_dir(), "ChunksV4");
    }
}

#[test]
fn the_sample_game_has_the_shapes_the_plan_asks_for() {
    let b = build(&spec());
    let m = &b.manifest;
    let by = |n: &str| m.files.iter().find(|f| f.name.contains(n)).unwrap();
    assert_eq!(by("pakchunk0").parts.len(), 3, "a file spanning 3 chunks");
    assert!(by("Game.exe").executable());
    assert_eq!(by("empty.txt").size(), 0);
    assert!(by("Ünïcødé ✓").name.ends_with("intro.bk2"), "UTF-16 name survives");
    let first = by("Game.exe").parts[0].guid;
    assert_eq!(by("pakchunk0").parts[0].guid, first, "one chunk shared by 2 files");
    // compressed and stored chunks both present
    let stored: Vec<u8> = b.chunks.values().map(|c| c[40]).collect();
    assert!(stored.contains(&0) && stored.contains(&1));
    assert_eq!(m.install_size(), m.files.iter().map(|f| f.size()).sum::<u64>());
}

#[test]
fn chunks_decode_and_are_checked() {
    let b = build(&spec());
    for c in &b.manifest.chunks {
        let file = &b.chunks[&c.path("ChunksV4")];
        let h = chunk::parse_header(file).unwrap();
        assert_eq!((h.header_version, h.header_size, h.guid), (3, 66, c.guid));
        let data = chunk::decode(file, c).unwrap();
        assert_eq!(data.len(), 4096);
    }
    let c = &b.manifest.chunks[0];
    let good = b.chunks[&c.path("ChunksV4")].clone();
    // wrong guid requested
    let mut other = c.clone();
    other.guid = Guid([1, 2, 3, 4]);
    assert!(matches!(chunk::decode(&good, &other), Err(Error::Format(_))));
    // wrong hash in the manifest
    let mut other = c.clone();
    other.sha1[0] ^= 1;
    assert!(chunk::decode(&good, &other).is_err());
    // wrong window size
    let mut other = c.clone();
    other.window_size = 2048;
    assert!(chunk::decode(&good, &other).is_err());
    // flipped data byte (stored chunk, so the header hash catches it)
    let c1 = &b.manifest.chunks[1];
    let mut bad = b.chunks[&c1.path("ChunksV4")].clone();
    let n = bad.len();
    bad[n - 1] ^= 0xFF;
    assert!(chunk::decode(&bad, c1).is_err());
    // truncated
    assert!(chunk::decode(&good[..40], c).is_err());
    assert!(chunk::decode(&good[..good.len() - 3], c).is_err());
    // bad magic
    let mut bad = good.clone();
    bad[0] = 0;
    assert!(chunk::decode(&bad, c).is_err());
    // an old header version without a hash still decodes when the manifest hash matches
    let raw = chunk::decode(&good, c).unwrap();
    let v3 = chunk_file(c.guid, c.rolling_hash, &raw, false);
    assert_eq!(chunk::decode(&v3, c).unwrap(), raw);
}

#[test]
fn header_and_section_checks() {
    let b = build(&spec());
    let good = b.manifest_bytes.clone();
    manifest::parse(&good).unwrap();
    // body hash
    let mut bad = good.clone();
    bad[16] ^= 1;
    assert!(matches!(manifest::parse(&bad), Err(Error::Format(_))));
    // uncompressed size wrong
    let mut bad = good.clone();
    bad[8] ^= 1;
    assert!(manifest::parse(&bad).is_err());
    // truncated anywhere
    for cut in [3, 20, 41, good.len() / 2, good.len() - 1] {
        assert!(manifest::parse(&good[..cut]).is_err(), "cut at {cut}");
    }
    // a section that claims more than the body holds
    let m = &b.manifest;
    let raw = write_manifest(m, false, 2);
    let mut bad = raw.clone();
    let body = 41;
    bad[body..body + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    fix_hash(&mut bad);
    assert!(manifest::parse(&bad).is_err());
    // a chunk count far larger than the data must not allocate, just fail
    let meta_size = u32::from_le_bytes(raw[body..body + 4].try_into().unwrap()) as usize;
    let mut bad = raw.clone();
    let at = body + meta_size + 5;
    bad[at..at + 4].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
    fix_hash(&mut bad);
    assert!(manifest::parse(&bad).is_err());
    // the untouched uncompressed form still parses
    let mut ok = raw.clone();
    fix_hash(&mut ok);
    assert_eq!(manifest::parse(&ok).unwrap().files.len(), m.files.len());
}

/// Recompute the header hash of an uncompressed manifest after editing its body.
fn fix_hash(m: &mut [u8]) {
    let h = manifest::sha1(&m[41..]);
    m[16..36].copy_from_slice(&h);
}

#[test]
fn part_outside_its_chunk_is_refused() {
    let mut m = build(&spec()).manifest;
    m.files[0].parts[0].offset = 4095;
    let bytes = write_manifest(&m, true, 2);
    assert!(manifest::parse(&bytes).is_err());
    let mut m = build(&spec()).manifest;
    m.files[0].parts[0].guid = Guid([9, 9, 9, 9]);
    assert!(manifest::parse(&write_manifest(&m, true, 2)).is_err());
}
