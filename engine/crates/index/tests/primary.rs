use canonical_core::Error;
use canonical_index::*;
use std::fs;

fn entry(namespace: u64, id: u64, revision: u64, sequence: u64, deleted: bool) -> IndexEntry {
    IndexEntry {
        key: IndexKey {
            namespace,
            id,
            revision,
        },
        location: Location {
            commit_sequence: sequence,
            frame_offset: 16 + sequence * 64,
            member: 0,
            deleted,
        },
    }
}

#[test]
fn identity_history_latest_tombstone_and_namespaces_are_distinct() {
    let mut index = PrimaryIndex::new();
    index.insert(entry(1, 500, 1, 1, false)).unwrap();
    index.insert(entry(2, 500, 1, 1, false)).unwrap();
    index.insert(entry(1, 500, 2, 2, true)).unwrap();
    assert!(index.get(1, 500, None).unwrap().deleted);
    assert!(!index.get(2, 500, None).unwrap().deleted);
    assert_eq!(index.get(1, 500, Some(1)).unwrap().commit_sequence, 1);
    assert_eq!(index.get(1, 500, Some(3)), None);
    assert_eq!(index.get(1, 999, None), None);
    assert_eq!(index.len(), 3);
    assert!(!index.is_empty());
}

#[test]
fn binary_roundtrip_is_stable_and_replaces_previous_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("primary.cbi");
    let mut index = PrimaryIndex::new();
    for item in [
        entry(1, 1, 1, 1, false),
        entry(1, 1, 2, 3, true),
        entry(2, 1, 1, 2, false),
    ] {
        index.insert(item).unwrap();
    }
    index.save(&path, 3).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), 32 + 3 * 48 + 4);
    assert_eq!(PrimaryIndex::load(&path, 3).unwrap(), index);
    let old = fs::read(&path).unwrap();
    assert!(index.save(&path, 0).is_err());
    assert_eq!(fs::read(&path).unwrap(), old);
    index.save(&path, 3).unwrap();
    assert_eq!(fs::read(&path).unwrap(), old);
    index.insert(entry(1, 1, 3, 4, false)).unwrap();
    index.save(&path, 4).unwrap();
    assert_eq!(PrimaryIndex::load(&path, 4).unwrap(), index);
    assert!(matches!(PrimaryIndex::load(&path, 3), Err(Error::Conflict)));
}

#[test]
fn malformed_entries_are_rejected_without_modifying_index() {
    let mut index = PrimaryIndex::new();
    for bad in [
        IndexEntry {
            key: IndexKey {
                id: 0,
                ..entry(1, 1, 1, 1, false).key
            },
            ..entry(1, 1, 1, 1, false)
        },
        entry(1, 1, 0, 1, false),
        entry(1, 1, 2, 1, false),
        entry(1, 1, 1, 0, false),
        IndexEntry {
            location: Location {
                frame_offset: 0,
                ..entry(1, 1, 1, 1, false).location
            },
            ..entry(1, 1, 1, 1, false)
        },
        IndexEntry {
            location: Location {
                member: 1024,
                ..entry(1, 1, 1, 1, false).location
            },
            ..entry(1, 1, 1, 1, false)
        },
    ] {
        assert!(index.insert(bad).is_err());
        assert!(index.is_empty());
    }
    index.insert(entry(1, 1, 1, 2, false)).unwrap();
    let before = index.clone();
    for bad in [
        entry(1, 1, 1, 3, false),
        entry(1, 1, 2, 1, false),
        entry(1, 1, 2, 2, false),
        entry(1, 1, 3, 3, false),
    ] {
        assert!(index.insert(bad).is_err());
        assert_eq!(index, before);
    }
}

#[test]
fn each_byte_mutation_and_every_short_prefix_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("primary.cbi");
    let mut index = PrimaryIndex::new();
    index.insert(entry(1, 1, 1, 1, false)).unwrap();
    index.save(&path, 1).unwrap();
    let bytes = fs::read(&path).unwrap();
    for i in 0..bytes.len() {
        let mut bad = bytes.clone();
        bad[i] ^= 1;
        fs::write(&path, bad).unwrap();
        assert!(PrimaryIndex::load(&path, 1).is_err(), "mutation {i}");
    }
    for n in 0..bytes.len() {
        fs::write(&path, &bytes[..n]).unwrap();
        assert!(PrimaryIndex::load(&path, 1).is_err(), "cut {n}");
    }
}

#[test]
fn sizes_reserved_flags_versions_and_watermark_are_checked_even_with_valid_crc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("primary.cbi");
    let mut index = PrimaryIndex::new();
    index.insert(entry(1, 1, 1, 1, false)).unwrap();
    index.save(&path, 1).unwrap();
    let bytes = fs::read(&path).unwrap();
    let rewrite = |mut data: Vec<u8>| {
        let end = data.len() - 4;
        let crc = crc32fast::hash(&data[..end]);
        data[end..].copy_from_slice(&crc.to_le_bytes());
        fs::write(&path, data).unwrap();
    };
    let mut bad = bytes.clone();
    bad[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
    rewrite(bad);
    assert!(PrimaryIndex::load(&path, 1).is_err());
    let mut bad = bytes.clone();
    bad[8..10].copy_from_slice(&99u16.to_le_bytes());
    rewrite(bad);
    assert!(matches!(
        PrimaryIndex::load(&path, 1),
        Err(Error::UnsupportedVersion(99))
    ));
    for offset in [10, 32 + 45, 32 + 46, 32 + 47] {
        let mut bad = bytes.clone();
        bad[offset] = 1;
        rewrite(bad);
        assert!(PrimaryIndex::load(&path, 1).is_err());
    }
    for (start, data) in [
        (32 + 8, 0u64.to_le_bytes()),  // zero record ID
        (32 + 16, 2u64.to_le_bytes()), // initial revision is not one
        (32 + 24, 0u64.to_le_bytes()), // zero commit sequence
        (32 + 32, 0u64.to_le_bytes()), // offset precedes segment header
    ] {
        let mut bad = bytes.clone();
        bad[start..start + 8].copy_from_slice(&data);
        rewrite(bad);
        assert!(PrimaryIndex::load(&path, 1).is_err());
    }
    let mut bad = bytes.clone();
    bad[32 + 40..32 + 44].copy_from_slice(&1024u32.to_le_bytes());
    rewrite(bad);
    assert!(PrimaryIndex::load(&path, 1).is_err());
    let mut bad = bytes.clone();
    bad[32 + 44] = 2;
    rewrite(bad);
    assert!(PrimaryIndex::load(&path, 1).is_err());
    let mut bad = bytes.clone();
    bad[16..24].copy_from_slice(&0u64.to_le_bytes());
    rewrite(bad);
    assert!(PrimaryIndex::load(&path, 0).is_err());
    assert!(index.save(&path, 0).is_err());
}

#[test]
fn oversized_file_is_rejected_before_reading_body_and_empty_index_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("primary.cbi");
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_INDEX_BYTES + 1).unwrap();
    drop(file);
    assert!(matches!(PrimaryIndex::load(&path, 0), Err(Error::TooLarge)));
    let index = PrimaryIndex::new();
    index.save(&path, 0).unwrap();
    assert_eq!(PrimaryIndex::load(&path, 0).unwrap(), index);
}
