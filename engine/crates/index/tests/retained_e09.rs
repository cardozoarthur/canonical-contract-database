use canonical_core::Error;
use canonical_index::{IndexEntry, IndexKey, Location, PrimaryIndex};
use std::fs;

fn entry(id: u64, revision: u64, sequence: u64) -> IndexEntry {
    IndexEntry {
        key: IndexKey {
            namespace: 1,
            id,
            revision,
        },
        location: Location {
            commit_sequence: sequence,
            frame_offset: 16 + sequence * 64,
            member: 0,
            deleted: false,
        },
    }
}
fn rewrite_crc(path: &std::path::Path, mut bytes: Vec<u8>) {
    let end = bytes.len() - 4;
    let checksum = crc32fast::hash(&bytes[..end]);
    bytes[end..].copy_from_slice(&checksum.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

#[test]
fn contiguous_retained_insertions_keep_the_published_v1_encoding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.cbi");
    let mut strict = PrimaryIndex::new();
    let mut retained = PrimaryIndex::new();
    for item in [entry(1, 1, 1), entry(1, 2, 2), entry(2, 1, 3)] {
        strict.insert(item).unwrap();
        retained.insert_retained(item).unwrap();
    }
    strict.save(&path, 3).unwrap();
    let published = fs::read(&path).unwrap();
    assert_eq!(&published[..10], b"CBRIDX01\x01\x00");
    retained.save(&path, 3).unwrap();
    assert_eq!(fs::read(&path).unwrap(), published);
    assert_eq!(PrimaryIndex::load(&path, 3).unwrap(), strict);
    PrimaryIndex::new().save(&path, 0).unwrap();
    assert_eq!(&fs::read(&path).unwrap()[8..10], &1u16.to_le_bytes());
}

#[test]
fn first_or_internal_revision_gap_requires_v2_and_roundtrips_without_lying_about_v1() {
    for revisions in [vec![3, 4], vec![1, 4]] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.cbi");
        let mut index = PrimaryIndex::new();
        for (sequence, revision) in revisions.into_iter().enumerate() {
            index
                .insert_retained(entry(1, revision, sequence as u64 + 1))
                .unwrap();
        }
        index.save(&path, 2).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"CBRIDX01");
        assert_eq!(&bytes[8..10], &2u16.to_le_bytes());
        assert_eq!(PrimaryIndex::load(&path, 2).unwrap(), index);
        let mut false_v1 = bytes;
        false_v1[8..10].copy_from_slice(&1u16.to_le_bytes());
        rewrite_crc(&path, false_v1);
        assert!(matches!(
            PrimaryIndex::load(&path, 2),
            Err(Error::Corruption { .. })
        ));
    }
}

#[test]
fn v2_still_checks_versions_reserved_fields_order_and_physical_monotonicity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.cbi");
    let mut index = PrimaryIndex::new();
    index.insert_retained(entry(1, 3, 1)).unwrap();
    index.insert_retained(entry(1, 6, 2)).unwrap();
    index.save(&path, 2).unwrap();
    let original = fs::read(&path).unwrap();
    let second = 32 + 48;
    for (offset, replacement) in [
        (10, vec![1]),
        (32 + 45, vec![1]),
        (second + 44, vec![2]),
        (second + 16, 3u64.to_le_bytes().to_vec()),
        (second + 24, 1u64.to_le_bytes().to_vec()),
        (second + 24, 0u64.to_le_bytes().to_vec()),
        (second + 32, 80u64.to_le_bytes().to_vec()),
        (second + 32, 0u64.to_le_bytes().to_vec()),
    ] {
        let mut bytes = original.clone();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        rewrite_crc(&path, bytes);
        assert!(PrimaryIndex::load(&path, 2).is_err(), "offset {offset}");
    }
    let mut bytes = original;
    bytes[8..10].copy_from_slice(&99u16.to_le_bytes());
    rewrite_crc(&path, bytes);
    assert!(matches!(
        PrimaryIndex::load(&path, 2),
        Err(Error::UnsupportedVersion(99))
    ));
}
