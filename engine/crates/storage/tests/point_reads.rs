use canonical_core::Error;
use canonical_storage::{StorageStats, Store};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
};

#[test]
fn offsets_and_point_reads_follow_append_and_reopen_without_scan() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"first").unwrap();
    store.append(b"second").unwrap();
    let second_offset = 16 + 28 + b"first".len() as u64;
    assert_eq!(store.frame_offset(0), None);
    assert_eq!(store.frame_offset(u64::MAX), None);
    assert_eq!(store.frame_offset(1), Some(16));
    assert_eq!(store.frame_offset(2), Some(second_offset));
    assert_eq!(store.frame_offset(3), None);
    assert_eq!(store.io_stats(), StorageStats::default());
    let second = store.read_frame_at(second_offset).unwrap();
    assert_eq!(second.sequence, 2);
    assert_eq!(second.payload, b"second");
    assert_eq!(store.io_stats().point_reads, 1);
    assert_eq!(store.io_stats().point_bytes, 28 + b"second".len() as u64);
    assert_eq!(store.io_stats().replay_frames, 0);
    // The read cursor must not change where the next append lands.
    store.read_frame_at(16).unwrap();
    store.append(b"third").unwrap();
    let third_offset = second_offset + 28 + b"second".len() as u64;
    assert_eq!(store.frame_offset(3), Some(third_offset));
    drop(store);
    let mut reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.io_stats().replay_frames, 3);
    assert_eq!(reopened.io_stats().point_reads, 0);
    assert_eq!(
        reopened.read_frame_at(third_offset).unwrap().payload,
        b"third"
    );
    assert_eq!(reopened.read_frame_at(16).unwrap().payload, b"first");
    assert_eq!(reopened.io_stats().replay_frames, 3);
    assert_eq!(reopened.io_stats().point_reads, 2);
}

#[test]
fn nonboundary_offsets_are_rejected_before_physical_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"payload with CBRF inside").unwrap();
    let stats = store.io_stats();
    for offset in [0, 15, 17, 40, u64::MAX] {
        assert!(matches!(
            store.read_frame_at(offset),
            Err(Error::InvalidInput(_))
        ));
    }
    assert_eq!(store.io_stats(), stats);
    assert!(store.ensure_healthy().is_ok());
}

#[test]
fn post_open_corruption_is_detected_on_disk_instead_of_serving_cached_payload() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"original").unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let mut attacker = fs::OpenOptions::new().write(true).open(path).unwrap();
    attacker.seek(SeekFrom::Start(16 + 24)).unwrap();
    attacker.write_all(b"X").unwrap();
    attacker.sync_all().unwrap();
    assert!(matches!(
        store.read_frame_at(16),
        Err(Error::Corruption { offset: 16 })
    ));
    assert_eq!(store.io_stats().point_reads, 1);
    assert_eq!(store.io_stats().point_bytes, 28 + b"original".len() as u64);
    assert!(matches!(store.ensure_healthy(), Err(Error::Poisoned)));
}

#[test]
fn short_point_read_counts_only_bytes_actually_read_and_poisons_writer() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"original").unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let attacker = fs::OpenOptions::new().write(true).open(path).unwrap();
    attacker.set_len(16 + 10).unwrap();
    assert!(matches!(
        store.read_frame_at(16),
        Err(Error::IncompleteTail { offset: 16 })
    ));
    assert_eq!(store.io_stats().point_reads, 1);
    assert_eq!(store.io_stats().point_bytes, 10);
    assert!(matches!(store.append(b"forbidden"), Err(Error::Poisoned)));
}

#[test]
fn recovery_counts_validated_frames_in_both_replay_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"confirmed").unwrap();
    store.append(b"cut").unwrap();
    drop(store);
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    bytes.pop();
    fs::write(path, bytes).unwrap();
    let (mut recovered, report) = Store::recover(dir.path()).unwrap();
    assert!(report.discarded_bytes > 0);
    assert_eq!(recovered.io_stats().replay_frames, 2);
    assert_eq!(recovered.frame_offset(1), Some(16));
    assert_eq!(recovered.frame_offset(2), None);
    assert_eq!(recovered.read_frame_at(16).unwrap().payload, b"confirmed");
    assert_eq!(recovered.io_stats().replay_frames, 2);
}
