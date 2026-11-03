use canonical_core::Error;
use canonical_query::Subject;
use canonical_registry::*;
use canonical_relations::*;
use std::collections::BTreeSet;

fn sr(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn key(id: u64) -> Reference {
    Reference { namespace: 1, id }
}
fn reader() -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    }
}
fn writer() -> WriteAccess {
    WriteAccess {
        subject: reader(),
        writable_namespaces: BTreeSet::from([1]),
    }
}
fn edge(id: u64, schema: u64, kind: u64, source: Reference, target: Reference) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: sr(schema),
        values: vec![Some(Value::Relation {
            source,
            kind,
            target,
        })],
    }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("ns{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    for id in 1..=6 {
        r.register_schema(Schema {
            reference: sr(id),
            fields: vec![Field {
                label: "value".into(),
                kind: if id >= 4 {
                    FieldType::Relation
                } else {
                    FieldType::UInt
                },
                optional: false,
            }],
        })
        .unwrap();
    }
    for (id, kind, source, target) in [(4, 10, 1, 2), (5, 11, 2, 3), (6, 12, 2, 1)] {
        r.register_relation_rule(RelationRule {
            schema: sr(id),
            kind,
            source_schema: sr(source),
            target_schema: sr(target),
        })
        .unwrap();
    }
    let records = vec![
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
        NewRecord {
            namespace: 1,
            id: 3,
            schema: sr(3),
            values: vec![Some(Value::UInt(3))],
        },
        NewRecord {
            namespace: 2,
            id: 2,
            schema: sr(2),
            values: vec![Some(Value::UInt(22))],
        },
    ];
    r.create_batch([1; 16], &records).unwrap();
    r
}

#[test]
fn task_action_file_and_reverse_are_reconstructed_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    link(&mut r, &writer(), edge(100, 4, 10, key(1), key(2)), 0).unwrap();
    link(&mut r, &writer(), edge(101, 5, 11, key(2), key(3)), 0).unwrap();
    let old = GraphView::new(&r, r.snapshot()).unwrap();
    assert_eq!(
        old.outgoing(&reader(), key(1), Some(sr(4)), 10)
            .unwrap()
            .edges[0]
            .target,
        key(2)
    );
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    let view = GraphView::new(&r, r.snapshot()).unwrap();
    assert_eq!(
        view.outgoing(&reader(), key(2), None, 10).unwrap().edges[0].target,
        key(3)
    );
    assert_eq!(
        view.incoming(&reader(), key(3), None, 10).unwrap().edges[0].source,
        key(2)
    );
    assert_eq!(
        view.incoming(&reader(), key(2), None, 10).unwrap().edges[0].source,
        key(1)
    );
    let reverse = view
        .walk(
            &reader(),
            key(3),
            &WalkOptions {
                direction: Direction::Incoming,
                relation_schema: None,
                max_depth: 16,
                max_nodes: 10,
                max_edges: 10,
            },
        )
        .unwrap();
    assert_eq!(reverse.nodes, vec![key(3), key(2), key(1)]);
    assert_eq!(
        view.outgoing(&reader(), key(1), None, 10).unwrap().edges,
        old.outgoing(&reader(), key(1), None, 10).unwrap().edges
    );
}

#[test]
fn access_and_endpoint_type_validation_reject_before_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let before = r.snapshot().commit_sequence;
    let denied = WriteAccess {
        subject: reader(),
        writable_namespaces: BTreeSet::new(),
    };
    assert!(matches!(
        link(&mut r, &denied, edge(100, 4, 10, key(1), key(2)), 0),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        link(
            &mut r,
            &writer(),
            edge(
                100,
                4,
                10,
                key(1),
                Reference {
                    namespace: 2,
                    id: 2
                }
            ),
            0
        ),
        Err(Error::Unauthorized)
    ));
    assert!(link(&mut r, &writer(), edge(100, 4, 10, key(99), key(2)), 0).is_err());
    assert!(link(&mut r, &writer(), edge(100, 4, 999, key(1), key(2)), 0).is_err());
    assert!(link(&mut r, &writer(), edge(100, 4, 10, key(2), key(1)), 0).is_err());
    assert_eq!(r.snapshot().commit_sequence, before);
    assert!(r.record(1, 100).is_none());
}

#[test]
fn hidden_cross_namespace_edges_do_not_leak_or_set_visible_has_more() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    link(&mut r, &writer(), edge(100, 4, 10, key(1), key(2)), 0).unwrap();
    let broad = WriteAccess {
        subject: Subject {
            id: "broad".into(),
            readable_namespaces: BTreeSet::from([1, 2]),
        },
        writable_namespaces: BTreeSet::from([1]),
    };
    link(
        &mut r,
        &broad,
        edge(
            101,
            4,
            10,
            key(1),
            Reference {
                namespace: 2,
                id: 2,
            },
        ),
        0,
    )
    .unwrap();
    let view = GraphView::new(&r, r.snapshot()).unwrap();
    let visible = view.outgoing(&reader(), key(1), None, 1).unwrap();
    assert_eq!(visible.edges.len(), 1);
    assert!(!visible.has_more);
    assert_eq!(visible.edges[0].reference, key(100));
    assert!(matches!(
        view.incoming(
            &reader(),
            Reference {
                namespace: 2,
                id: 2
            },
            None,
            10
        ),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        unlink(&mut r, &writer(), key(101), 1),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        link(&mut r, &writer(), edge(101, 4, 10, key(1), key(2)), 1),
        Err(Error::Unauthorized)
    ));
}

#[test]
fn cycle_walk_is_bounded_and_old_snapshot_stays_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    for e in [
        edge(100, 4, 10, key(1), key(2)),
        edge(101, 5, 11, key(2), key(3)),
        edge(102, 6, 12, key(2), key(1)),
    ] {
        link(&mut r, &writer(), e, 0).unwrap();
    }
    let old = GraphView::new(&r, r.snapshot()).unwrap();
    let options = WalkOptions {
        direction: Direction::Outgoing,
        relation_schema: None,
        max_depth: 16,
        max_nodes: 100,
        max_edges: 100,
    };
    let walk = old.walk(&reader(), key(1), &options).unwrap();
    assert_eq!(
        walk.nodes.iter().copied().collect::<BTreeSet<_>>(),
        BTreeSet::from([key(1), key(2), key(3)])
    );
    assert_eq!(walk.edges.len(), 3);
    assert!(!walk.truncated);
    let limited = old
        .walk(
            &reader(),
            key(1),
            &WalkOptions {
                max_nodes: 2,
                ..options
            },
        )
        .unwrap();
    assert!(limited.nodes.len() <= 2);
    assert!(limited.truncated);
    assert!(old.outgoing(&reader(), key(1), None, 0).is_err());
    unlink(&mut r, &writer(), key(101), 1).unwrap();
    r.delete(1, 3, 1).unwrap();
    let new = GraphView::new(&r, r.snapshot()).unwrap();
    assert!(
        new.incoming(&reader(), key(2), Some(sr(4)), 10)
            .unwrap()
            .edges
            .len()
            == 1
    );
    assert_eq!(
        old.outgoing(&reader(), key(2), Some(sr(5)), 10)
            .unwrap()
            .edges
            .len(),
        1
    );
    assert!(
        new.outgoing(&reader(), key(2), Some(sr(5)), 10)
            .unwrap()
            .edges
            .is_empty()
    );
}

#[test]
fn frozen_native_rule_journal_rebuilds_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("segments")).unwrap();
    std::fs::write(
        dir.path().join("manifest.cbr"),
        include_bytes!("fixtures/native-v1/manifest.cbr"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("segments/00000001.cbr"),
        include_bytes!("fixtures/native-v1/segments/00000001.cbr"),
    )
    .unwrap();
    let r = Registry::open(dir.path()).unwrap();
    for layout in [
        canonical_relations::GraphLayout::Legacy,
        canonical_relations::GraphLayout::Compact,
    ] {
        let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
        assert_eq!(
            view.outgoing(&reader(), key(1), Some(sr(4)), 10)
                .unwrap()
                .edges[0]
                .target,
            key(2)
        );
        assert_eq!(
            view.incoming(&reader(), key(3), Some(sr(5)), 10)
                .unwrap()
                .edges[0]
                .source,
            key(2)
        );
    }
}

#[test]
fn general_authorized_queries_cannot_bypass_native_endpoint_visibility() {
    use canonical_query::{Order, Predicate, Query, QueryEngine};
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let broad = WriteAccess {
        subject: Subject {
            id: "broad".into(),
            readable_namespaces: BTreeSet::from([1, 2]),
        },
        writable_namespaces: BTreeSet::from([1]),
    };
    link(
        &mut r,
        &broad,
        edge(
            100,
            4,
            10,
            key(1),
            Reference {
                namespace: 2,
                id: 2,
            },
        ),
        0,
    )
    .unwrap();
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let request = Query {
        namespaces: vec![1],
        schemas: vec![sr(4)],
        filter: Predicate::True,
        order: Order::default(),
        limit: 100,
        cursor: None,
    };
    let page = engine.query(&reader(), &request).unwrap();
    assert!(
        page.records.is_empty(),
        "QueryEngine exposed a native edge hidden by GraphView"
    );
    assert_eq!(page.plan.candidates, 0);
    assert_eq!(page.plan.rows_examined, 0);
    assert_eq!(
        engine
            .query(&broad.subject, &request)
            .unwrap()
            .records
            .len(),
        1
    );
}

#[test]
fn query_visibility_uses_the_supplied_snapshot_not_latest_edge_metadata() {
    use canonical_query::{Order, Predicate, Query, QueryEngine};
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    link(&mut r, &writer(), edge(100, 4, 10, key(1), key(2)), 0).unwrap();
    let old = r.snapshot();
    let broad = WriteAccess {
        subject: Subject {
            id: "broad".into(),
            readable_namespaces: BTreeSet::from([1, 2]),
        },
        writable_namespaces: BTreeSet::from([1]),
    };
    link(
        &mut r,
        &broad,
        edge(
            100,
            4,
            10,
            key(1),
            Reference {
                namespace: 2,
                id: 2,
            },
        ),
        1,
    )
    .unwrap();
    let request = Query {
        namespaces: vec![1],
        schemas: vec![sr(4)],
        filter: Predicate::True,
        order: Order::default(),
        limit: 100,
        cursor: None,
    };
    let mut previous = QueryEngine::new(&r, old).unwrap();
    let mut current = QueryEngine::new(&r, r.snapshot()).unwrap();
    let old_page = previous.query(&reader(), &request).unwrap();
    assert_eq!(old_page.records.len(), 1);
    assert_eq!(old_page.records[0].revision, 1);
    assert!(
        current
            .query(&reader(), &request)
            .unwrap()
            .records
            .is_empty()
    );
}
