use canonical_core::Error;
use canonical_storage::Store;
use std::{fs, process::Command};

#[test]
fn durable_ack_survives_separate_processes() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.bin");
    let output = dir.path().join("out.bin");
    let db = dir.path().join("db");
    let data: Vec<u8> = (0..=255).cycle().take(4096).collect();
    fs::write(&input, &data).unwrap();
    let run = |args: &[&std::ffi::OsStr]| {
        let result = Command::new(env!("CARGO_BIN_EXE_canonical-store"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    run(&["append".as_ref(), db.as_os_str(), input.as_os_str()]);
    run(&[
        "read".as_ref(),
        db.as_os_str(),
        "1".as_ref(),
        output.as_os_str(),
    ]);
    assert_eq!(fs::read(output).unwrap(), data);
}

#[test]
fn lock_is_exclusive_and_released_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    assert!(matches!(Store::open(dir.path()), Err(Error::WriterBusy)));
    drop(store);
    Store::open(dir.path()).unwrap();
}

#[test]
fn partial_tail_and_corruption_are_never_published() {
    let original = tempfile::tempdir().unwrap();
    {
        let mut db = Store::open(original.path()).unwrap();
        db.append(b"first").unwrap();
        db.append(b"second").unwrap();
    }
    let path = original.path().join("segments/00000001.cbr");
    let bytes = fs::read(&path).unwrap();
    // Every cut inside the second frame must be rejected; a complete prefix is valid.
    let first_end = 16 + 24 + 5 + 4;
    for cut in first_end + 1..bytes.len() {
        fs::write(&path, &bytes[..cut]).unwrap();
        assert!(
            matches!(
                Store::open(original.path()),
                Err(Error::IncompleteTail { .. })
            ),
            "cut={cut}"
        );
    }
    let mut bad = bytes.clone();
    bad[first_end - 5] ^= 0x40;
    fs::write(&path, bad).unwrap();
    assert!(matches!(
        Store::open(original.path()),
        Err(Error::Corruption { .. })
    ));
    fs::write(&path, &bytes[..first_end]).unwrap();
    assert_eq!(Store::open(original.path()).unwrap().snapshot().len(), 1);
}

#[test]
fn empty_payload_multiple_commits_and_snapshot_are_supported() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = Store::open(dir.path()).unwrap();
    assert_eq!(db.append(b"").unwrap().sequence, 1);
    let snap = db.snapshot();
    assert_eq!(db.append(b"\0hello\xff").unwrap().sequence, 2);
    assert_eq!(snap.len(), 1);
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert_eq!(db.snapshot()[1].payload, b"\0hello\xff");
}

#[test]
fn unsupported_manifest_and_missing_segment_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let manifest = dir.path().join("manifest.cbr");
    let original = fs::read(&manifest).unwrap();
    let mut unknown = original.clone();
    unknown[8..10].copy_from_slice(&99u16.to_le_bytes());
    fs::write(&manifest, unknown).unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(Error::UnsupportedVersion(99))
    ));
    fs::write(manifest, original).unwrap();
    fs::remove_file(dir.path().join("segments/00000001.cbr")).unwrap();
    assert!(Store::open(dir.path()).is_err());
}

#[test]
fn closed_backup_can_be_restored_and_extended() {
    let source = tempfile::tempdir().unwrap();
    {
        let mut store = Store::open(source.path()).unwrap();
        store.append(b"backup-check").unwrap();
    }
    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("segments")).unwrap();
    for name in ["manifest.cbr", "segments/00000001.cbr"] {
        fs::copy(source.path().join(name), destination.path().join(name)).unwrap();
    }
    let mut restored = Store::open(destination.path()).unwrap();
    assert_eq!(restored.snapshot()[0].payload, b"backup-check");
    assert_eq!(restored.append(b"after-restore").unwrap().sequence, 2);
}

#[test]
fn oversized_append_does_not_change_persistent_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"preserve").unwrap();
    let original = fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    assert!(matches!(
        store.append(&vec![0; canonical_core::MAX_FRAME_BYTES]),
        Err(Error::TooLarge)
    ));
    assert_eq!(store.snapshot().len(), 1);
    assert_eq!(
        original,
        fs::read(dir.path().join("segments/00000001.cbr")).unwrap()
    );
    assert_eq!(store.append(b"still-usable").unwrap().sequence, 2);
}

#[test]
fn orphan_segment_is_preserved_without_silent_reinitialization() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("segments")).unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    fs::write(&path, b"must not overwrite").unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(Error::InvalidInput(_))
    ));
    assert_eq!(fs::read(path).unwrap(), b"must not overwrite");
}
