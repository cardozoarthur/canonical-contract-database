use canonical_core::Error;
use canonical_index::{CacheFormat, IndexEntry, PrimaryIndex};
use canonical_registry::*;
use std::fs;
const POLICIES: [IndexPolicy; 4] = [
    IndexPolicy::Full,
    IndexPolicy::Grouped,
    IndexPolicy::Delta,
    IndexPolicy::ReplayOnly,
];
fn sr() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn input(id: u64, value: u64) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: sr(),
        values: vec![Some(Value::UInt(value))],
    }
}
fn fixture(path: &std::path::Path, policy: IndexPolicy) -> Registry {
    let mut r = Registry::open_with_index_policy(path, policy).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "policy".into(),
        level: Level::Local,
    })
    .unwrap();
    r.register_schema(Schema {
        reference: sr(),
        fields: vec![Field {
            label: "number".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    r.create_batch([1; 16], &[input(1, 10), input(2, 20)])
        .unwrap();
    r.update(input(1, 30), 1).unwrap();
    r.delete(1, 2, 1).unwrap();
    r
}
fn cache(path: &std::path::Path) -> std::path::PathBuf {
    path.join("indexes/primary.cbi")
}
fn format(policy: IndexPolicy) -> CacheFormat {
    match policy {
        IndexPolicy::Full => CacheFormat::Legacy,
        IndexPolicy::Grouped => CacheFormat::Grouped,
        IndexPolicy::Delta => CacheFormat::Delta,
        IndexPolicy::ReplayOnly => panic!("no replay-only file"),
    }
}
#[test]
fn policy_roundtrip_keeps_data_lookup_and_legacy_default_while_cache_is_optional() {
    for policy in POLICIES {
        let d = tempfile::tempdir().unwrap();
        let mut r = fixture(d.path(), policy);
        let sequence = r.snapshot().commit_sequence;
        let expected = r.history(1, 1).to_vec();
        r.flush_index().unwrap();
        assert_eq!(r.index_policy(), policy);
        assert_eq!(cache(d.path()).exists(), policy != IndexPolicy::ReplayOnly);
        drop(r);
        let mut r = Registry::open_with_index_policy(d.path(), policy).unwrap();
        assert_eq!(r.snapshot().commit_sequence, sequence);
        assert_eq!(r.history(1, 1), expected);
        assert_eq!(r.index_status().loaded, policy != IndexPolicy::ReplayOnly);
        let found = r.lookup(1, 1, Some(2)).unwrap();
        assert_eq!(found.revision.unwrap(), expected[1]);
        assert_eq!(found.metrics.point_reads, 1);
        assert_eq!(found.metrics.frames_scanned, 0);
        assert!(r.lookup(1, 2, None).unwrap().revision.is_none());
        drop(r);
        let r = Registry::open(d.path()).unwrap();
        assert_eq!(r.index_policy(), IndexPolicy::Full);
        assert_eq!(r.record(1, 1).unwrap().revision, 2);
    }
}
#[test]
fn crc_valid_wrong_content_falls_back_for_every_cache_format_without_changing_journal() {
    for policy in POLICIES[..3].iter().copied() {
        let d = tempfile::tempdir().unwrap();
        let mut r = fixture(d.path(), policy);
        let watermark = r.snapshot().commit_sequence;
        r.flush_index().unwrap();
        drop(r);
        let index = PrimaryIndex::load(&cache(d.path()), watermark).unwrap();
        let mut changed = PrimaryIndex::new();
        for entry in index.entries() {
            let mut location = entry.location;
            if entry.key.id == 1 && entry.key.revision == 1 {
                location.member = 1;
            }
            changed
                .insert(IndexEntry {
                    key: entry.key,
                    location,
                })
                .unwrap();
        }
        changed
            .save_with_format(&cache(d.path()), watermark, format(policy))
            .unwrap();
        let journal = d.path().join("segments/00000001.cbr");
        let before = fs::read(&journal).unwrap();
        let mut r = Registry::open_with_index_policy(d.path(), policy).unwrap();
        assert_eq!(
            r.index_status().reason,
            Some(IndexRebuildReason::ContentMismatch)
        );
        assert_eq!(
            r.lookup(1, 1, Some(1))
                .unwrap()
                .revision
                .unwrap()
                .record
                .payload,
            r.revision(1, 1, 1).unwrap().record.payload
        );
        assert_eq!(fs::read(journal).unwrap(), before);
    }
}
#[test]
fn replay_only_preserves_existing_cache_and_checkpoint_still_runs() {
    let d = tempfile::tempdir().unwrap();
    let mut r = fixture(d.path(), IndexPolicy::Full);
    r.flush_index().unwrap();
    drop(r);
    let bytes = fs::read(cache(d.path())).unwrap();
    let mut r = Registry::open_with_index_policy(d.path(), IndexPolicy::ReplayOnly).unwrap();
    assert!(!r.index_status().loaded);
    assert!(r.index_status().rebuilt);
    assert_eq!(r.index_status().reason, None);
    r.flush_index().unwrap();
    r.checkpoint().unwrap();
    assert!(d.path().join("checkpoint.cbr").is_file());
    assert_eq!(fs::read(cache(d.path())).unwrap(), bytes);
    drop(r);
    fs::write(cache(d.path()), b"not an index").unwrap();
    let mut r = Registry::open_with_index_policy(d.path(), IndexPolicy::ReplayOnly).unwrap();
    r.flush_index().unwrap();
    assert_eq!(fs::read(cache(d.path())).unwrap(), b"not an index");
    assert_eq!(
        r.lookup(1, 1, None)
            .unwrap()
            .revision
            .unwrap()
            .record
            .revision,
        2
    );
}
#[test]
fn compaction_and_recovery_preserve_policy_cache_generation_and_sparse_history() {
    for policy in POLICIES {
        let d = tempfile::tempdir().unwrap();
        let mut r = fixture(d.path(), policy);
        r.update(input(1, 40), 2).unwrap();
        r.flush_index().unwrap();
        let receipt = r.transaction_receipt([1; 16]).unwrap();
        r.compact(Retention::KeepLast(1)).unwrap();
        assert_eq!(r.index_policy(), policy);
        r.flush_index().unwrap();
        assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::HistoryPruned)));
        drop(r);
        let (mut r, recovery) = Registry::recover_with_index_policy(d.path(), policy).unwrap();
        assert_eq!(recovery.discarded_bytes, 0);
        assert_eq!(r.index_policy(), policy);
        assert_eq!(r.transaction_receipt([1; 16]), Some(receipt));
        assert_eq!(
            r.lookup(1, 1, None)
                .unwrap()
                .revision
                .unwrap()
                .record
                .revision,
            3
        );
        r.update(input(1, 50), 3).unwrap();
    }
}

#[test]
fn corrupt_cache_cannot_hide_a_bad_journal_and_recover_repairs_only_partial_tail() {
    use std::io::Write;
    for policy in POLICIES[..3].iter().copied() {
        let d = tempfile::tempdir().unwrap();
        let mut r = fixture(d.path(), policy);
        let watermark = r.snapshot().commit_sequence;
        r.flush_index().unwrap();
        drop(r);
        let original_cache = fs::read(cache(d.path())).unwrap();
        let journal = d.path().join("segments/00000001.cbr");
        let prefix = fs::read(&journal).unwrap();
        let mut bad = original_cache.clone();
        *bad.last_mut().unwrap() ^= 1;
        fs::write(cache(d.path()), bad).unwrap();
        let r = Registry::open_with_index_policy(d.path(), policy).unwrap();
        assert_eq!(
            r.index_status().reason,
            Some(IndexRebuildReason::InvalidOrStale)
        );
        drop(r);
        assert_eq!(fs::read(&journal).unwrap(), prefix);
        fs::write(cache(d.path()), &original_cache).unwrap();
        let frame = canonical_codec::encode_frame(watermark + 1, b"incomplete").unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&journal).unwrap();
        file.write_all(&frame[..25]).unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert!(matches!(
            Registry::open_with_index_policy(d.path(), policy),
            Err(Error::IncompleteTail { .. })
        ));
        let (mut r, report) = Registry::recover_with_index_policy(d.path(), policy).unwrap();
        assert_eq!(report.discarded_bytes, 25);
        assert!(report.backup.unwrap().exists());
        assert_eq!(fs::read(&journal).unwrap(), prefix);
        assert!(r.index_status().loaded);
        assert_eq!(
            r.lookup(1, 1, None)
                .unwrap()
                .revision
                .unwrap()
                .record
                .revision,
            2
        );
        drop(r);
        let mut corrupt = prefix.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        fs::write(&journal, &corrupt).unwrap();
        assert!(matches!(
            Registry::recover_with_index_policy(d.path(), policy),
            Err(Error::Corruption { .. })
        ));
        assert_eq!(fs::read(&journal).unwrap(), corrupt);
    }
}
#[test]
fn missing_or_unavailable_cache_falls_back_and_cache_failure_does_not_poison_data() {
    let d = tempfile::tempdir().unwrap();
    let mut r = fixture(d.path(), IndexPolicy::Delta);
    r.flush_index().unwrap();
    drop(r);
    fs::remove_file(cache(d.path())).unwrap();
    let mut r = Registry::open_with_index_policy(d.path(), IndexPolicy::Grouped).unwrap();
    assert_eq!(r.index_status().reason, Some(IndexRebuildReason::Missing));
    r.flush_index().unwrap();
    drop(r);
    fs::rename(cache(d.path()), d.path().join("indexes/saved.cbi")).unwrap();
    fs::create_dir(cache(d.path())).unwrap();
    let mut r = Registry::open_with_index_policy(d.path(), IndexPolicy::Full).unwrap();
    assert_eq!(
        r.index_status().reason,
        Some(IndexRebuildReason::Unavailable)
    );
    assert!(r.flush_index().is_err());
    r.update(input(1, 44), 2).unwrap();
    assert_eq!(r.record(1, 1).unwrap().revision, 3);
}
#[test]
fn replay_only_noop_flush_cannot_bypass_a_poisoned_writer() {
    use canonical_storage::AppendStage;
    let d = tempfile::tempdir().unwrap();
    let mut r = fixture(d.path(), IndexPolicy::ReplayOnly);
    let result = r.create_batch_with_hook([9; 16], &[input(3, 30)], |stage| {
        if stage == AppendStage::BeforeSync {
            Err(Error::InvalidInput("injected"))
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert!(matches!(r.flush_index(), Err(Error::Poisoned)));
    assert!(matches!(r.checkpoint(), Err(Error::Poisoned)));
    assert!(!cache(d.path()).exists());
}
#[test]
fn closed_compacted_backup_restores_with_any_policy_and_legacy_flush_stays_legacy() {
    for policy in POLICIES[..3].iter().copied() {
        let d = tempfile::tempdir().unwrap();
        let mut r = fixture(d.path(), policy);
        r.update(input(1, 40), 2).unwrap();
        let receipt = r.transaction_receipt([1; 16]).unwrap();
        let generation = r
            .compact(Retention::KeepLast(1))
            .unwrap()
            .generation
            .generation;
        r.flush_index().unwrap();
        drop(r);
        let destination = tempfile::tempdir().unwrap();
        fs::create_dir(destination.path().join("segments")).unwrap();
        fs::create_dir(destination.path().join("indexes")).unwrap();
        let segment = format!("segments/{generation:08}.cbr");
        for name in ["manifest.cbr", segment.as_str(), "indexes/primary.cbi"] {
            fs::copy(d.path().join(name), destination.path().join(name)).unwrap();
        }
        let mut restored =
            Registry::open_with_index_policy(destination.path(), IndexPolicy::Full).unwrap();
        assert!(restored.index_status().loaded);
        assert_eq!(restored.transaction_receipt([1; 16]), Some(receipt));
        assert!(matches!(
            restored.lookup(1, 1, Some(1)),
            Err(Error::HistoryPruned)
        ));
        restored.flush_index().unwrap();
        let bytes = fs::read(cache(destination.path())).unwrap();
        assert_eq!(&bytes[..8], b"CBRIDX01");
        assert_eq!(u16::from_le_bytes(bytes[8..10].try_into().unwrap()), 2);
        restored.update(input(1, 50), 3).unwrap();
    }
}
