use canonical_core::Error;
use canonical_storage::{AppendStage, Store};
use std::{fs, io};

#[test]
fn recovery_preserves_backup_before_discarding_incomplete_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"confirmed").unwrap();
    store.append(b"incomplete").unwrap();
    drop(store);
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 3);
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(Error::IncompleteTail { .. })
    ));
    let (mut recovered, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(
        report.discarded_bytes,
        (24 + b"incomplete".len() + 1) as u64
    );
    assert_eq!(fs::read(report.backup.unwrap()).unwrap(), bytes);
    assert_eq!(recovered.snapshot().len(), 1);
    assert_eq!(recovered.snapshot()[0].payload, b"confirmed");
    assert_eq!(recovered.append(b"after recovery").unwrap().sequence, 2);
    drop(recovered);
    let (replayed, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.discarded_bytes, 0);
    assert!(report.backup.is_none());
    assert_eq!(replayed.snapshot().len(), 2);
}

#[test]
fn recovery_never_truncates_intermediate_corruption_or_incomplete_segment_header() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"first").unwrap();
    store.append(b"second").unwrap();
    drop(store);
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    bytes[16 + 24] ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Store::recover(dir.path()),
        Err(Error::Corruption { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    fs::write(&path, &bytes[..8]).unwrap();
    assert!(matches!(
        Store::recover(dir.path()),
        Err(Error::IncompleteTail { offset: 0 })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes[..8]);
}

#[test]
fn incomplete_frame_with_invalid_sequence_is_corruption_not_repairable_tail() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    let mut frame = canonical_codec::encode_frame(2, b"partial").unwrap();
    frame.truncate(24);
    bytes.extend_from_slice(&frame);
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Store::recover(dir.path()),
        Err(Error::Corruption { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn recovery_backup_does_not_overwrite_existing_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"prior").unwrap();
    drop(store);
    let reserved = dir.path().join("recovery-0000.segment.bak");
    fs::write(&reserved, b"earlier backup evidence").unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"partial");
    fs::write(&path, &bytes).unwrap();
    let (_, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.discarded_bytes, 7);
    assert_ne!(report.backup.as_ref().unwrap(), &reserved);
    assert_eq!(fs::read(report.backup.unwrap()).unwrap(), bytes);
    assert_eq!(fs::read(reserved).unwrap(), b"earlier backup evidence");
}

#[test]
fn every_cut_inside_last_frame_recovers_exact_confirmed_prefix() {
    let source = tempfile::tempdir().unwrap();
    let mut store = Store::open(source.path()).unwrap();
    store.append(b"first").unwrap();
    store.append(b"last-frame").unwrap();
    drop(store);
    let original = fs::read(source.path().join("segments/00000001.cbr")).unwrap();
    let first_end = 16 + 24 + b"first".len() + 4;
    for cut in first_end + 1..original.len() {
        let dir = tempfile::tempdir().unwrap();
        drop(Store::open(dir.path()).unwrap());
        let path = dir.path().join("segments/00000001.cbr");
        fs::write(&path, &original[..cut]).unwrap();
        let (recovered, report) = Store::recover(dir.path()).unwrap();
        assert_eq!(
            report.discarded_bytes,
            (cut - first_end) as u64,
            "cut={cut}"
        );
        assert_eq!(recovered.snapshot().len(), 1, "cut={cut}");
        assert_eq!(recovered.snapshot()[0].payload, b"first", "cut={cut}");
        assert_eq!(fs::read(&path).unwrap(), original[..first_end], "cut={cut}");
        assert_eq!(
            fs::read(report.backup.unwrap()).unwrap(),
            original[..cut],
            "cut={cut}"
        );
    }
}

#[test]
fn checkpoint_can_be_replaced_after_later_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"first").unwrap();
    store.checkpoint().unwrap();
    let old = fs::read(dir.path().join("checkpoint.cbr")).unwrap();
    store.append(b"second").unwrap();
    store.checkpoint().unwrap();
    assert_ne!(fs::read(dir.path().join("checkpoint.cbr")).unwrap(), old);
    drop(store);
    let (store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(true));
    assert_eq!(store.snapshot().len(), 2);
}

#[test]
fn checkpoint_validates_against_log_and_invalid_checkpoint_cannot_mask_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"first").unwrap();
    store.checkpoint().unwrap();
    drop(store);
    let (mut store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(true));
    store.append(b"second").unwrap();
    drop(store);
    let (store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(true));
    assert_eq!(store.snapshot().len(), 2);
    drop(store);
    let checkpoint = dir.path().join("checkpoint.cbr");
    fs::write(&checkpoint, b"corrupt checkpoint").unwrap();
    let (store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(false));
    assert_eq!(store.snapshot().len(), 2);
    drop(store);
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    bytes[16 + 24] ^= 1;
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        Store::recover(dir.path()),
        Err(Error::Corruption { .. })
    ));
}

#[test]
fn hook_failure_requires_reopen_and_sync_precedes_publication() {
    for failure in [
        AppendStage::Prepared,
        AppendStage::HeaderWritten,
        AppendStage::BodyWritten,
        AppendStage::BeforeSync,
        AppendStage::Synced,
        AppendStage::Published,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.append(b"prior").unwrap();
        let result = store.append_with_hook(b"candidate", |stage| {
            if stage == failure {
                Err(io::Error::other("injected I/O failure").into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert!(matches!(store.ensure_healthy(), Err(Error::Poisoned)));
        assert!(matches!(store.append(b"forbidden"), Err(Error::Poisoned)));
        assert!(matches!(store.checkpoint(), Err(Error::Poisoned)));
        drop(store);
        let (store, _) = Store::recover(dir.path()).unwrap();
        let snapshot = store.snapshot();
        assert!(snapshot.len() == 1 || snapshot.len() == 2);
        if matches!(failure, AppendStage::Synced | AppendStage::Published) {
            assert_eq!(snapshot.len(), 2);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut observed = Vec::new();
    store
        .append_with_hook(b"ordered", |stage| {
            observed.push(stage);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        observed,
        [
            AppendStage::Prepared,
            AppendStage::HeaderWritten,
            AppendStage::BodyWritten,
            AppendStage::BeforeSync,
            AppendStage::Synced,
            AppendStage::Published
        ]
    );
}
