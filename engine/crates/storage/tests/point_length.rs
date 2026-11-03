use canonical_core::Error;
use canonical_storage::Store;
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
};

#[test]
fn valid_but_changed_frame_length_is_rejected_against_known_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"original").unwrap();
    store.append(b"second").unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    // A frame with a valid checksum and the right sequence can still not replace
    // a different-sized immutable frame whose boundary was established at open.
    let changed = canonical_codec::encode_frame(1, b"x").unwrap();
    let mut attacker = fs::OpenOptions::new().write(true).open(path).unwrap();
    attacker.seek(SeekFrom::Start(16)).unwrap();
    attacker.write_all(&changed).unwrap();
    attacker.sync_all().unwrap();
    assert!(matches!(
        store.read_frame_at(16),
        Err(Error::Corruption { offset: 16 })
    ));
    assert_eq!(store.io_stats().point_bytes, 24);
    assert!(matches!(store.ensure_healthy(), Err(Error::Poisoned)));
}
