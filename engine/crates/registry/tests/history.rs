use canonical_core::Error;
use canonical_registry::*;
use canonical_storage::AppendStage;
use std::sync::{Arc, Barrier, Mutex};

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
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "history".into(),
        level: Level::Local,
    })
    .unwrap();
    r.register_schema(Schema {
        reference: sr(),
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    r.create_batch([1; 16], &[input(1, 10), input(2, 20)])
        .unwrap();
    r
}

#[test]
fn competing_expected_revisions_allow_exactly_one_update() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Arc::new(Mutex::new(setup(dir.path())));
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for value in [30, 40] {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            registry.lock().unwrap().update(input(1, value), 1)
        }));
    }
    barrier.wait();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Conflict)))
            .count(),
        1
    );
    assert_eq!(registry.lock().unwrap().history(1, 1).len(), 2);
}

#[test]
fn history_tombstones_and_snapshots_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let old = r.snapshot();
    let sequence = old.commit_sequence;
    let updated = r.update(input(1, 30), 1).unwrap();
    assert_eq!(updated.revision, 2);
    let deletion = r.delete(1, 1, 2).unwrap();
    assert_eq!(deletion.record.revision, 3);
    assert!(deletion.deleted);
    assert!(r.record(1, 1).is_none());
    assert_eq!(r.history(1, 1).len(), 3);
    assert_eq!(
        r.decode_values(sr(), &old.record(1, 1).unwrap().payload)
            .unwrap(),
        vec![Some(Value::UInt(10))]
    );
    assert_eq!(
        r.snapshot_at(sequence).unwrap().record(1, 1),
        old.record(1, 1)
    );
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert!(r.record(1, 1).is_none());
    assert_eq!(
        r.history(1, 1)
            .iter()
            .map(|v| v.record.revision)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(r.revision(1, 1, 3).unwrap().deleted);
    assert_eq!(
        r.snapshot_at(sequence).unwrap().record(1, 1),
        old.record(1, 1)
    );
    assert!(r.snapshot_at(u64::MAX).is_err());
}

#[test]
fn create_reuse_and_missing_delete_are_rejected_but_restore_is_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    assert!(matches!(
        r.create(1, 1, sr(), &[Some(Value::UInt(1))]),
        Err(Error::Conflict)
    ));
    assert!(matches!(r.delete(1, 999, 1), Err(Error::NotFound)));
    r.delete(1, 1, 1).unwrap();
    assert!(matches!(
        r.create(1, 1, sr(), &[Some(Value::UInt(1))]),
        Err(Error::Conflict)
    ));
    assert!(matches!(r.delete(1, 1, 2), Err(Error::Conflict)));
    assert!(matches!(r.update(input(1, 99), 2), Err(Error::Conflict)));
    assert!(matches!(r.restore(input(1, 99), 1), Err(Error::Conflict)));
    assert_eq!(r.restore(input(1, 99), 2).unwrap().revision, 3);
    assert_eq!(r.decode_record(1, 1).unwrap(), vec![Some(Value::UInt(99))]);
}

#[test]
fn mutation_conflict_rejects_whole_batch_and_retry_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let changes = [
        Mutation::Update {
            record: input(1, 30),
            expected_revision: 1,
        },
        Mutation::Delete {
            namespace: 1,
            id: 2,
            expected_revision: 2,
        },
    ];
    assert!(matches!(r.commit([2; 16], &changes), Err(Error::Conflict)));
    assert_eq!(r.record(1, 1).unwrap().revision, 1);
    assert_eq!(r.history(1, 1).len(), 1);
    let changes = [
        Mutation::Update {
            record: input(1, 30),
            expected_revision: 1,
        },
        Mutation::Delete {
            namespace: 1,
            id: 2,
            expected_revision: 1,
        },
    ];
    let receipt = r.commit([2; 16], &changes).unwrap();
    assert_eq!(r.commit([2; 16], &changes).unwrap(), receipt);
    assert_eq!(r.history(1, 1).len(), 2);
    assert!(r.record(1, 2).is_none());
    drop(r);
    let mut r = Registry::open(dir.path()).unwrap();
    assert_eq!(r.commit([2; 16], &changes).unwrap(), receipt);
    assert_eq!(r.history(1, 2).len(), 2);
}

#[test]
fn failure_after_sync_recovers_whole_mutation_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let snap = r.snapshot();
    let mutations = [
        Mutation::Update {
            record: input(1, 30),
            expected_revision: 1,
        },
        Mutation::Delete {
            namespace: 1,
            id: 2,
            expected_revision: 1,
        },
    ];
    let result = r.commit_with_hook([8; 16], &mutations, |stage| {
        if stage == AppendStage::Synced {
            Err(std::io::Error::other("failure after sync").into())
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert_eq!(r.snapshot().len(), snap.len());
    assert_eq!(r.record(1, 1).unwrap().revision, 1);
    drop(r);
    let mut r = Registry::open(dir.path()).unwrap();
    assert_eq!(r.record(1, 1).unwrap().revision, 2);
    assert!(r.record(1, 2).is_none());
    let receipt = r.transaction_receipt([8; 16]).unwrap();
    assert_eq!(r.commit([8; 16], &mutations).unwrap(), receipt);
}

#[test]
fn snapshots_cover_atomic_changes_schema_evolution_and_compensating_restore() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let first = r.snapshot();
    let new_schema = SchemaRef { version: 2, ..sr() };
    r.register_schema(Schema {
        reference: new_schema,
        fields: vec![
            Field {
                label: "value".into(),
                kind: FieldType::UInt,
                optional: false,
            },
            Field {
                label: "note".into(),
                kind: FieldType::Utf8,
                optional: true,
            },
        ],
    })
    .unwrap();
    let evolved = NewRecord {
        namespace: 1,
        id: 1,
        schema: new_schema,
        values: vec![Some(Value::UInt(50)), Some(Value::Utf8("evolved".into()))],
    };
    let receipt = r
        .commit(
            [10; 16],
            &[
                Mutation::Update {
                    record: evolved,
                    expected_revision: 1,
                },
                Mutation::Delete {
                    namespace: 1,
                    id: 2,
                    expected_revision: 1,
                },
            ],
        )
        .unwrap();
    let prior = r.snapshot_at(receipt.commit_sequence - 1).unwrap();
    assert_eq!(prior.record(1, 1), first.record(1, 1));
    assert_eq!(prior.record(1, 2), first.record(1, 2));
    assert_eq!(
        r.history(1, 1)[1].commit_sequence,
        r.history(1, 2)[1].commit_sequence
    );
    let deleted = r.snapshot();
    r.restore(input(2, 20), 2).unwrap();
    assert_eq!(
        r.snapshot_at(deleted.commit_sequence).unwrap().record(1, 2),
        None
    );
    assert!(deleted.record(1, 2).is_none());
    assert_eq!(
        r.decode_values(
            r.revision(1, 1, 1).unwrap().record.schema,
            &r.revision(1, 1, 1).unwrap().record.payload
        )
        .unwrap(),
        vec![Some(Value::UInt(10))]
    );
    assert!(r.snapshot_at(0).unwrap().is_empty());
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert_eq!(
        r.snapshot_at(first.commit_sequence).unwrap().record(1, 1),
        first.record(1, 1)
    );
    assert_eq!(r.history(1, 2).len(), 3);
}

#[test]
fn transaction_ids_are_unique_across_old_and_new_commit_apis() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    assert!(matches!(
        r.commit([1; 16], &[Mutation::Create(input(3, 30))]),
        Err(Error::Conflict)
    ));
    r.commit([2; 16], &[Mutation::Create(input(3, 30))])
        .unwrap();
    assert!(matches!(
        r.create_batch([2; 16], &[input(4, 40)]),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        r.commit(
            [3; 16],
            &[
                Mutation::Update {
                    record: input(1, 50),
                    expected_revision: 1
                },
                Mutation::Delete {
                    namespace: 1,
                    id: 1,
                    expected_revision: 1
                }
            ]
        ),
        Err(Error::Conflict)
    ));
    assert_eq!(r.record(1, 1).unwrap().revision, 1);
    assert!(r.update(input(1, 99), u64::MAX).is_err());
}

#[test]
fn frozen_history_journal_preserves_legacy_and_revision_event_tags() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("segments")).unwrap();
    std::fs::write(
        dir.path().join("manifest.cbr"),
        include_bytes!("fixtures/history-v1/manifest.cbr"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("segments/00000001.cbr"),
        include_bytes!("fixtures/history-v1/segments/00000001.cbr"),
    )
    .unwrap();
    let r = Registry::open(dir.path()).unwrap();
    assert!(r.record(1, 1).is_none());
    assert_eq!(r.history(1, 1).len(), 3);
    assert_eq!(r.transaction_receipt([1; 16]).unwrap().commit_sequence, 3);
    assert_eq!(r.transaction_receipt([2; 16]).unwrap().commit_sequence, 4);
    assert_eq!(r.snapshot_at(4).unwrap().record(1, 1).unwrap().revision, 2);
    assert!(r.revision(1, 1, 3).unwrap().deleted);
}
