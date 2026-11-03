use canonical_core::Error;
use canonical_storage::{RewriteStage, Store};
use std::{fs, process::Command};

const STAGES: [RewriteStage; 6] = [
    RewriteStage::Prepared,
    RewriteStage::SegmentWritten,
    RewriteStage::SegmentSynced,
    RewriteStage::ManifestWritten,
    RewriteStage::ManifestPublished,
    RewriteStage::Activated,
];

#[test]
fn rewrites_preserve_sequences_and_backups_then_cleanup_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old-long-payload").unwrap();
    store.append(b"other-payload").unwrap();
    store.checkpoint().unwrap();
    assert_eq!(store.generation(), 1);
    let old = store.active_segment_path();
    let report = store.rewrite(&[b"new".to_vec(), b"x".to_vec()]).unwrap();
    assert_eq!(
        (report.previous_generation, report.generation, report.frames),
        (1, 2, 2)
    );
    assert!(report.active_bytes < report.previous_active_bytes);
    assert_eq!(
        report.total_segment_bytes,
        report.active_bytes + report.previous_active_bytes
    );
    assert!(old.exists());
    assert_eq!(store.snapshot()[1].sequence, 2);
    assert_eq!(store.append(b"third").unwrap().sequence, 3);
    let offset = store.frame_offset(1).unwrap();
    assert_eq!(store.read_frame_at(offset).unwrap().payload, b"new");
    fs::write(dir.path().join("segments/notes.txt"), b"keep").unwrap();
    let cleaned = store.cleanup_obsolete_generations().unwrap();
    assert_eq!(cleaned.removed_generations, vec![1]);
    assert_eq!(cleaned.removed_bytes, report.previous_active_bytes);
    assert!(!old.exists());
    assert!(store.active_segment_path().exists());
    assert!(dir.path().join("segments/notes.txt").exists());
    drop(store);
    let mut reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.generation(), 2);
    assert_eq!(reopened.snapshot()[0].payload, b"new");
    assert_eq!(reopened.append(b"fourth").unwrap().sequence, 4);
}

#[test]
fn invalid_rewrite_does_not_change_generation_or_poison_writer() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old").unwrap();
    assert!(matches!(store.rewrite(&[]), Err(Error::InvalidInput(_))));
    assert_eq!(store.generation(), 1);
    assert_eq!(store.append(b"next").unwrap().sequence, 2);
}

#[test]
fn hook_errors_poison_and_reopen_selects_old_or_new_complete_generation() {
    for stage in STAGES {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.append(b"old").unwrap();
        let result = store.rewrite_with_hook(&[b"new".to_vec()], |seen| {
            if seen == stage {
                Err(Error::InvalidInput("injected"))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err(), "{stage:?}");
        assert!(matches!(store.append(b"bad"), Err(Error::Poisoned)));
        drop(store);
        let mut reopened = Store::open(dir.path()).unwrap();
        let published = matches!(
            stage,
            RewriteStage::ManifestPublished | RewriteStage::Activated
        );
        assert_eq!(
            reopened.snapshot()[0].payload,
            if published { b"new" } else { b"old" }
        );
        assert_eq!(reopened.append(b"next").unwrap().sequence, 2);
        let report = reopened
            .rewrite(&[b"newer".to_vec(), b"next".to_vec()])
            .unwrap();
        assert!(report.generation >= 2);
    }
}

#[test]
fn process_exit_at_each_stage_keeps_all_acknowledged_commits() {
    for stage in STAGES {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.append(b"old").unwrap();
        drop(store);
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "rewrite_process_child", "--ignored"])
            .env("CBR_REWRITE_CHILD_DIR", dir.path())
            .env("CBR_REWRITE_CHILD_STAGE", format!("{stage:?}"))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(73), "{stage:?}");
        let mut reopened = Store::open(dir.path()).unwrap();
        let published = matches!(
            stage,
            RewriteStage::ManifestPublished | RewriteStage::Activated
        );
        assert_eq!(reopened.snapshot().len(), 1);
        assert_eq!(
            reopened.snapshot()[0].payload,
            if published { b"new" } else { b"old" }
        );
        assert_eq!(reopened.append(b"next").unwrap().sequence, 2);
    }
}

#[test]
#[ignore = "invoked only as crash-injection child"]
fn rewrite_process_child() {
    let path = std::env::var_os("CBR_REWRITE_CHILD_DIR").expect("child path");
    let stage = std::env::var("CBR_REWRITE_CHILD_STAGE").unwrap();
    let mut store = Store::open(path).unwrap();
    store
        .rewrite_with_hook(&[b"new".to_vec()], |seen| {
            if format!("{seen:?}") == stage {
                std::process::exit(73);
            }
            Ok(())
        })
        .unwrap();
    panic!("expected failpoint reached");
}

#[test]
fn corrupt_manifest_or_active_generation_never_falls_back_to_backup() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old").unwrap();
    store.rewrite(&[b"new".to_vec()]).unwrap();
    let active = store.active_segment_path();
    drop(store);
    let mut bytes = fs::read(&active).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&active, &bytes).unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(Error::Corruption { .. })
    ));
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(active, bytes).unwrap();
    let manifest = dir.path().join("manifest.cbr");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes[16] ^= 1;
    fs::write(&manifest, bytes).unwrap();
    assert!(matches!(
        Store::open(dir.path()),
        Err(Error::Corruption { .. })
    ));
}

#[test]
fn rewrite_and_cleanup_verify_current_disk_before_discarding_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old").unwrap();
    store.rewrite(&[b"new".to_vec()]).unwrap();
    let active = store.active_segment_path();
    let mut bytes = fs::read(&active).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(active, bytes).unwrap();
    assert!(store.cleanup_obsolete_generations().is_err());
    assert!(dir.path().join("segments/00000001.cbr").exists());
    assert!(matches!(
        store.rewrite(&[b"hidden corruption".to_vec()]),
        Err(Error::Poisoned)
    ));
}

#[test]
fn orphan_evidence_and_shared_old_snapshot_survive_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old snapshot").unwrap();
    let snapshot = store.snapshot();
    let orphan = dir.path().join("segments/00000002.cbr");
    fs::write(&orphan, b"incomplete orphan evidence").unwrap();
    let report = store.rewrite(&[b"new".to_vec()]).unwrap();
    assert_eq!(report.generation, 3);
    assert_eq!(snapshot[0].payload, b"old snapshot");
    assert_eq!(fs::read(&orphan).unwrap(), b"incomplete orphan evidence");
    let report = store.cleanup_obsolete_generations().unwrap();
    assert_eq!(report.removed_generations, vec![1]);
    assert!(orphan.exists());
}

#[test]
fn closed_generation_backup_restores_without_any_prior_generations() {
    let source = tempfile::tempdir().unwrap();
    let mut store = Store::open(source.path()).unwrap();
    store.append(b"old").unwrap();
    store.checkpoint().unwrap();
    store.rewrite(&[b"new".to_vec()]).unwrap();
    let active_name = store.active_segment_path().file_name().unwrap().to_owned();
    drop(store);
    let destination = tempfile::tempdir().unwrap();
    fs::create_dir(destination.path().join("segments")).unwrap();
    fs::copy(
        source.path().join("manifest.cbr"),
        destination.path().join("manifest.cbr"),
    )
    .unwrap();
    fs::copy(
        source.path().join("segments").join(&active_name),
        destination.path().join("segments").join(active_name),
    )
    .unwrap();
    fs::copy(
        source.path().join("checkpoint.cbr"),
        destination.path().join("checkpoint.cbr"),
    )
    .unwrap();
    let (mut restored, recovery) = Store::recover(destination.path()).unwrap();
    assert_eq!(recovery.checkpoint_valid, Some(false));
    assert_eq!(restored.generation(), 2);
    assert_eq!(restored.snapshot()[0].payload, b"new");
    assert_eq!(restored.append(b"after restore").unwrap().sequence, 2);
}

#[test]
fn oversized_rewrite_is_rejected_before_generation_creation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old").unwrap();
    assert!(matches!(
        store.rewrite(&[vec![0; canonical_core::MAX_FRAME_BYTES]]),
        Err(Error::TooLarge)
    ));
    assert_eq!(
        fs::read_dir(dir.path().join("segments")).unwrap().count(),
        1
    );
    assert_eq!(store.append(b"still healthy").unwrap().sequence, 2);
}

#[test]
fn checkpoint_v2_binds_the_generation_and_cannot_mask_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.append(b"old").unwrap();
    store.rewrite(&[b"new".to_vec()]).unwrap();
    store.checkpoint().unwrap();
    let checkpoint = dir.path().join("checkpoint.cbr");
    assert_eq!(fs::metadata(&checkpoint).unwrap().len(), 48);
    drop(store);
    let (mut store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(true));
    store.rewrite(&[b"xxx".to_vec()]).unwrap();
    drop(store);
    let (store, report) = Store::recover(dir.path()).unwrap();
    assert_eq!(report.checkpoint_valid, Some(false));
    let active = store.active_segment_path();
    drop(store);
    let mut bytes = fs::read(&active).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(active, bytes).unwrap();
    assert!(matches!(
        Store::recover(dir.path()),
        Err(Error::Corruption { .. })
    ));
}
