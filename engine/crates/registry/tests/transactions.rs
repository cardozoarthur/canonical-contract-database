use canonical_core::Error;
use canonical_registry::*;
use canonical_storage::AppendStage;
use std::{fs, process::Command};

fn schema() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn prepare(path: &std::path::Path) -> Registry {
    let mut registry = Registry::open(path).unwrap();
    registry
        .register_namespace(Namespace {
            id: 1,
            name: "txn".into(),
            level: Level::Local,
        })
        .unwrap();
    registry
        .register_schema(Schema {
            reference: schema(),
            fields: vec![Field {
                label: "value".into(),
                kind: FieldType::UInt,
                optional: false,
            }],
        })
        .unwrap();
    registry
}
fn input(id: u64) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: schema(),
        values: vec![Some(Value::UInt(id))],
    }
}

#[test]
fn batch_ack_and_retry_are_atomic_durable_and_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prepare(dir.path());
    let before = registry.snapshot();
    let receipt = registry
        .create_batch([1; 16], &[input(1), input(2)])
        .unwrap();
    assert_eq!(receipt.records, 2);
    assert_eq!(before.len(), 0);
    assert_eq!(registry.snapshot().len(), 2);
    assert_eq!(
        registry
            .create_batch([1; 16], &[input(1), input(2)])
            .unwrap(),
        receipt
    );
    assert!(matches!(
        registry.create_batch([1; 16], &[input(3)]),
        Err(Error::Conflict)
    ));
    drop(registry);
    for _ in 0..3 {
        let mut registry = Registry::open(dir.path()).unwrap();
        assert_eq!(registry.transaction_receipt([1; 16]), Some(receipt));
        assert_eq!(
            registry
                .create_batch([1; 16], &[input(1), input(2)])
                .unwrap(),
            receipt
        );
        assert_eq!(registry.snapshot().len(), 2);
    }
}

#[test]
fn invalid_or_duplicate_member_never_commits_a_partial_batch() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prepare(dir.path());
    let path = dir.path().join("segments/00000001.cbr");
    let bytes = fs::read(&path).unwrap();
    let invalid = NewRecord {
        values: vec![Some(Value::Bool(true))],
        ..input(2)
    };
    assert!(
        registry
            .create_batch([2; 16], &[input(1), invalid])
            .is_err()
    );
    assert!(
        registry
            .create_batch([2; 16], &[input(1), input(1)])
            .is_err()
    );
    assert!(registry.create_batch([0; 16], &[input(1)]).is_err());
    assert!(registry.create_batch([2; 16], &[]).is_err());
    assert!(
        registry
            .create_batch([2; 16], &vec![input(1); MAX_TRANSACTION_RECORDS + 1])
            .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(registry.snapshot().is_empty());
}

#[test]
fn indeterminate_io_requires_reopen_then_retry_by_transaction_id() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prepare(dir.path());
    let result = registry.create_batch_with_hook([3; 16], &[input(1), input(2)], |stage| {
        if stage == AppendStage::Synced {
            Err(std::io::Error::other("injected acknowledgment failure").into())
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    assert!(matches!(
        registry.create_batch([4; 16], &[input(3)]),
        Err(Error::Poisoned)
    ));
    assert!(matches!(
        registry.register_namespace(Namespace {
            id: 1,
            name: "txn".into(),
            level: Level::Local
        }),
        Err(Error::Poisoned)
    ));
    // Readers still see a consistent old prefix until re-open, never half the batch.
    assert!(registry.snapshot().is_empty());
    drop(registry);
    let mut registry = Registry::open(dir.path()).unwrap();
    assert_eq!(registry.snapshot().len(), 2);
    let receipt = registry.transaction_receipt([3; 16]).unwrap();
    assert_eq!(
        registry
            .create_batch([3; 16], &[input(1), input(2)])
            .unwrap(),
        receipt
    );
}

#[test]
fn child_exit_at_each_protocol_stage_recovers_all_or_nothing() {
    for stage in [
        "Prepared",
        "HeaderWritten",
        "BodyWritten",
        "BeforeSync",
        "Synced",
        "Published",
        "Ack",
    ] {
        let dir = tempfile::tempdir().unwrap();
        drop(prepare(dir.path()));
        let result = Command::new(env!("CARGO_BIN_EXE_transaction-probe"))
            .arg(dir.path())
            .arg(stage)
            .output()
            .unwrap();
        if stage == "Ack" {
            assert!(
                result.status.success(),
                "{stage}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        } else {
            assert_eq!(
                result.status.code(),
                Some(73),
                "{stage}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        let (mut registry, report) = Registry::recover(dir.path()).unwrap();
        let count = registry.snapshot().len();
        assert!(
            count == 0 || count == 2,
            "partial batch at {stage}: {count}"
        );
        if matches!(stage, "Synced" | "Published" | "Ack") {
            assert_eq!(count, 2, "lost synced commit at {stage}");
        }
        if stage == "Prepared" {
            assert_eq!(count, 0);
        }
        if stage == "HeaderWritten" {
            assert!(report.discarded_bytes > 0);
            assert!(report.backup.is_some());
        }
        let receipt = registry
            .create_batch([9; 16], &[input(10), input(11)])
            .unwrap();
        assert_eq!(registry.snapshot().len(), 2);
        assert_eq!(registry.transaction_receipt([9; 16]), Some(receipt));
    }
}

#[test]
fn every_cut_inside_a_transaction_restores_only_previous_commits() {
    let original = tempfile::tempdir().unwrap();
    let mut registry = prepare(original.path());
    registry
        .create(1, 99, schema(), &[Some(Value::UInt(99))])
        .unwrap();
    let segment = original.path().join("segments/00000001.cbr");
    let prefix = fs::metadata(&segment).unwrap().len() as usize;
    registry
        .create_batch([5; 16], &[input(1), input(2)])
        .unwrap();
    drop(registry);
    let bytes = fs::read(&segment).unwrap();
    for cut in prefix + 1..bytes.len() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("segments")).unwrap();
        fs::copy(
            original.path().join("manifest.cbr"),
            dir.path().join("manifest.cbr"),
        )
        .unwrap();
        fs::write(dir.path().join("segments/00000001.cbr"), &bytes[..cut]).unwrap();
        let (registry, report) = Registry::recover(dir.path()).unwrap();
        assert_eq!(registry.snapshot().len(), 1, "cut {cut}");
        assert_eq!(
            registry.decode_record(1, 99).unwrap(),
            vec![Some(Value::UInt(99))]
        );
        assert_eq!(registry.transaction_receipt([5; 16]), None);
        assert_eq!(report.discarded_bytes, (cut - prefix) as u64);
        assert_eq!(fs::read(report.backup.unwrap()).unwrap(), bytes[..cut]);
    }
}

#[test]
fn aggregate_batch_limit_is_rejected_without_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prepare(dir.path());
    let sr = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    registry
        .register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "data".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let before = fs::read(&path).unwrap();
    let records: Vec<_> = (1..=17)
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr,
            values: vec![Some(Value::Bytes(vec![0; MAX_TEXT_BYTES]))],
        })
        .collect();
    assert!(matches!(
        registry.create_batch([6; 16], &records),
        Err(Error::TooLarge)
    ));
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(registry.snapshot().is_empty());
    assert!(registry.create_batch([7; 16], &[input(1)]).is_ok());
}
