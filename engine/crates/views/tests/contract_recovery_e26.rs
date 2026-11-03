#[path = "../examples/support/contract_workflow.rs"]
mod workflow;
use canonical_registry::{Mutation, NewRecord, Reference, Registry, Value};
use canonical_storage::AppendStage;
fn change_task() -> NewRecord {
    NewRecord {
        namespace: 1,
        id: 101,
        schema: workflow::schema(2),
        values: vec![
            Some(Value::Utf8("task101".into())),
            Some(Value::Ref(Reference {
                namespace: 1,
                id: 1001,
            })),
            Some(Value::Enum(2)),
        ],
    }
}
fn edge(id: u64, schema: u64, target: u64) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: workflow::schema(schema),
        values: vec![Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 402,
            },
            kind: schema - 99,
            target: Reference {
                namespace: 1,
                id: target,
            },
        })],
    }
}
#[test]
fn workflow_ack_failure_preserves_prefix_then_reopens_complete_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("db");
    let artifacts = temp.path().join("artifacts");
    workflow::run(&db, &artifacts).unwrap();
    workflow::verify_rejected_changes(&db).unwrap();
    let mut registry = Registry::open(&db).unwrap();
    workflow::check_transition(&registry, 101, 1, 2).unwrap();
    let changes = vec![
        Mutation::Update {
            record: change_task(),
            expected_revision: 1,
        },
        Mutation::Create(NewRecord {
            namespace: 1,
            id: 402,
            schema: workflow::schema(4),
            values: vec![
                Some(Value::Utf8("activate101-after-recovery".into())),
                Some(Value::UInt(1)),
            ],
        }),
        Mutation::Create(edge(2301, 101, 101)),
        Mutation::Create(edge(2302, 102, 301)),
    ];
    let tx = [44; 16];
    let failure = registry.commit_with_hook(tx, &changes, |stage| {
        if stage == AppendStage::Synced {
            Err(std::io::Error::other("synthetic lost acknowledgement after sync").into())
        } else {
            Ok(())
        }
    });
    assert!(failure.is_err());
    assert_eq!(registry.record(1, 101).unwrap().revision, 1);
    assert!(registry.record(1, 402).is_none());
    assert!(matches!(
        registry.commit([45; 16], &changes),
        Err(canonical_core::Error::Poisoned)
    ));
    drop(registry);
    let mut reopened = Registry::open(&db).unwrap();
    let receipt = reopened
        .transaction_receipt(tx)
        .expect("synced transaction recovered");
    assert_eq!(receipt.records, 4);
    assert_eq!(
        reopened.decode_record(1, 101).unwrap(),
        change_task().values
    );
    for (id, revision) in [(101, 2), (402, 1), (2301, 1), (2302, 1)] {
        assert_eq!(
            reopened.revision(1, id, revision).unwrap().commit_sequence,
            receipt.commit_sequence
        );
    }
    assert_eq!(
        reopened
            .native_edge(Reference {
                namespace: 1,
                id: 2301
            })
            .unwrap()
            .target,
        Reference {
            namespace: 1,
            id: 101
        }
    );
    assert_eq!(
        reopened
            .native_edge(Reference {
                namespace: 1,
                id: 2302
            })
            .unwrap()
            .target,
        Reference {
            namespace: 1,
            id: 301
        }
    );
    assert_eq!(reopened.commit(tx, &changes).unwrap(), receipt);
    assert_eq!(reopened.history(1, 101).len(), 2);
}
