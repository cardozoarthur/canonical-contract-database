use canonical_storage::*;
#[test]
fn verified_point_read_returns_the_same_arc_only_after_physical_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.append(b"abc").unwrap();
    let mirror = s.snapshot()[0].clone();
    let read = s.read_verified_frame_at(16).unwrap();
    assert!(std::sync::Arc::ptr_eq(&mirror, &read));
    assert_eq!(s.io_stats().point_reads, 1);
    assert_eq!(s.io_stats().point_bytes, 31);
}
#[test]
fn crc_correct_disk_change_cannot_replace_the_verified_replay_mirror() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.append(b"original bytes").unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[40] ^= 1;
    let end = bytes.len() - 4;
    let crc = crc32fast::hash(&bytes[40..end]);
    bytes[end..].copy_from_slice(&crc.to_le_bytes());
    assert!(canonical_codec::decode_frame_ref(&bytes[16..], 16).is_ok());
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        s.read_verified_frame_at(16),
        Err(canonical_core::Error::Corruption { offset: 16 })
    ));
    assert!(matches!(
        s.append(b"forbidden"),
        Err(canonical_core::Error::Poisoned)
    ));
    assert_eq!(s.io_stats().point_bytes, 42);
}
#[test]
fn owned_api_still_compares_mirror_and_nonboundary_does_not_poison() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    s.append(b"abc").unwrap();
    assert!(matches!(
        s.read_verified_frame_at(17),
        Err(canonical_core::Error::InvalidInput(_))
    ));
    assert_eq!(s.io_stats().point_reads, 0);
    assert!(s.ensure_healthy().is_ok());
    let owned = s.read_frame_at(16).unwrap();
    assert_eq!(owned.payload, b"abc");
    assert!(s.ensure_healthy().is_ok());
}
