use canonical_core::Error;
use canonical_registry::*;

fn sr(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn values(source: Reference, target: Reference, kind: u64) -> Vec<Option<Value>> {
    vec![Some(Value::Relation {
        source,
        kind,
        target,
    })]
}
fn key(id: u64) -> Reference {
    Reference { namespace: 1, id }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "relations".into(),
        level: Level::Local,
    })
    .unwrap();
    for id in 1..=4 {
        r.register_schema(Schema {
            reference: sr(id),
            fields: vec![Field {
                label: "value".into(),
                kind: if id == 4 {
                    FieldType::Relation
                } else {
                    FieldType::UInt
                },
                optional: false,
            }],
        })
        .unwrap();
    }
    r.register_relation_rule(RelationRule {
        schema: sr(4),
        kind: 10,
        source_schema: sr(1),
        target_schema: sr(2),
    })
    .unwrap();
    r.create_batch(
        [1; 16],
        &[
            NewRecord {
                namespace: 1,
                id: 1,
                schema: sr(1),
                values: vec![Some(Value::UInt(1))],
            },
            NewRecord {
                namespace: 1,
                id: 2,
                schema: sr(2),
                values: vec![Some(Value::UInt(2))],
            },
        ],
    )
    .unwrap();
    r
}

#[test]
fn native_constraints_apply_even_through_raw_registry_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    assert!(matches!(
        r.create(1, 100, sr(4), &values(key(99), key(2), 10)),
        Err(Error::NotFound)
    ));
    assert!(
        r.create(1, 100, sr(4), &values(key(1), key(2), 999))
            .is_err()
    );
    assert!(
        r.create(1, 100, sr(4), &values(key(2), key(1), 10))
            .is_err()
    );
    r.create(1, 100, sr(4), &values(key(1), key(2), 10))
        .unwrap();
    assert!(matches!(r.delete(1, 1, 1), Err(Error::Conflict)));
    assert!(
        r.update(
            NewRecord {
                namespace: 1,
                id: 1,
                schema: sr(3),
                values: vec![Some(Value::UInt(3))]
            },
            1
        )
        .is_err()
    );
    assert_eq!(r.native_edge(key(100)).unwrap().target, key(2));
    assert_eq!(r.record(1, 1).unwrap().revision, 1);
}

#[test]
fn detach_and_vertex_delete_are_atomic_in_either_order_and_restore_is_checked() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.create(1, 100, sr(4), &values(key(1), key(2), 10))
        .unwrap();
    let mutations = [
        Mutation::Delete {
            namespace: 1,
            id: 1,
            expected_revision: 1,
        },
        Mutation::Delete {
            namespace: 1,
            id: 100,
            expected_revision: 1,
        },
    ];
    let receipt = r.commit([2; 16], &mutations).unwrap();
    assert!(r.record(1, 1).is_none());
    assert!(r.native_edge(key(100)).is_none());
    assert_eq!(r.commit([2; 16], &mutations).unwrap(), receipt);
    assert!(
        r.restore(
            NewRecord {
                namespace: 1,
                id: 100,
                schema: sr(4),
                values: values(key(1), key(2), 10)
            },
            2
        )
        .is_err()
    );
    r.commit(
        [3; 16],
        &[
            Mutation::Restore {
                record: NewRecord {
                    namespace: 1,
                    id: 100,
                    schema: sr(4),
                    values: values(key(1), key(2), 10),
                },
                expected_revision: 2,
            },
            Mutation::Restore {
                record: NewRecord {
                    namespace: 1,
                    id: 1,
                    schema: sr(1),
                    values: vec![Some(Value::UInt(1))],
                },
                expected_revision: 2,
            },
        ],
    )
    .unwrap();
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert_eq!(r.native_edge(key(100)).unwrap().revision, 3);
    assert_eq!(r.record(1, 1).unwrap().revision, 3);
}

#[test]
fn legacy_relation_values_are_not_reinterpreted_and_rules_are_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.register_schema(Schema {
        reference: sr(5),
        fields: vec![Field {
            label: "opaque".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.create(1, 200, sr(5), &values(key(99), key(98), 77))
        .unwrap();
    assert!(matches!(
        r.register_relation_rule(RelationRule {
            schema: sr(5),
            kind: 77,
            source_schema: sr(1),
            target_schema: sr(2)
        }),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        r.register_relation_rule(RelationRule {
            schema: sr(4),
            kind: 10,
            source_schema: sr(2),
            target_schema: sr(1)
        }),
        Err(Error::Conflict)
    ));
    assert!(r.native_edge(key(200)).is_none());
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert!(r.record(1, 200).is_some());
    assert!(r.relation_rule(sr(4)).is_some());
}

#[test]
fn moving_edge_releases_old_endpoint_and_protects_new_endpoint_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.create(1, 3, sr(2), &[Some(Value::UInt(3))]).unwrap();
    r.create(1, 100, sr(4), &values(key(1), key(2), 10))
        .unwrap();
    r.commit(
        [2; 16],
        &[
            Mutation::Delete {
                namespace: 1,
                id: 2,
                expected_revision: 1,
            },
            Mutation::Update {
                record: NewRecord {
                    namespace: 1,
                    id: 100,
                    schema: sr(4),
                    values: values(key(1), key(3), 10),
                },
                expected_revision: 1,
            },
        ],
    )
    .unwrap();
    assert!(r.record(1, 2).is_none());
    assert_eq!(r.native_edge(key(100)).unwrap().target, key(3));
    assert!(matches!(r.delete(1, 3, 1), Err(Error::Conflict)));
    drop(r);
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(matches!(r.delete(1, 3, 1), Err(Error::Conflict)));
}

#[test]
fn durable_detach_and_delete_recover_together_after_acknowledgment_failure() {
    use canonical_storage::AppendStage;
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.create(1, 100, sr(4), &values(key(1), key(2), 10))
        .unwrap();
    let changes = [
        Mutation::Delete {
            namespace: 1,
            id: 100,
            expected_revision: 1,
        },
        Mutation::Delete {
            namespace: 1,
            id: 1,
            expected_revision: 1,
        },
    ];
    assert!(
        r.commit_with_hook([2; 16], &changes, |stage| if stage == AppendStage::Synced {
            Err(std::io::Error::other("injected after sync").into())
        } else {
            Ok(())
        })
        .is_err()
    );
    assert!(r.native_edge(key(100)).is_some());
    assert!(r.record(1, 1).is_some());
    drop(r);
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(r.native_edge(key(100)).is_none());
    assert!(r.record(1, 1).is_none());
    let receipt = r.transaction_receipt([2; 16]).unwrap();
    assert_eq!(r.commit([2; 16], &changes).unwrap(), receipt);
}
