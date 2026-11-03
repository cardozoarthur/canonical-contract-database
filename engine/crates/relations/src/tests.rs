use super::*;
use canonical_registry::{
    Field, FieldType, Level, Mutation, Namespace, RelationRule, Schema, Value,
};

fn reference(namespace: u64, id: u64) -> Reference {
    Reference { namespace, id }
}
fn node_schema() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn edge_schema() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    }
}
fn subject(scopes: &[u64]) -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: scopes.iter().copied().collect(),
    }
}
fn access(scopes: &[u64], write: &[u64]) -> WriteAccess {
    WriteAccess {
        subject: subject(scopes),
        writable_namespaces: write.iter().copied().collect(),
    }
}
fn edge(id: u64, source: Reference, target: Reference) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: edge_schema(),
        values: vec![Some(Value::Relation {
            source,
            kind: 7,
            target,
        })],
    }
}
fn setup() -> (tempfile::TempDir, Registry) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("n{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    r.register_schema(Schema {
        reference: node_schema(),
        fields: vec![Field {
            label: "name".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    r.register_schema(Schema {
        reference: edge_schema(),
        fields: vec![Field {
            label: "relation".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.register_relation_rule(RelationRule {
        schema: edge_schema(),
        kind: 7,
        source_schema: node_schema(),
        target_schema: node_schema(),
    })
    .unwrap();
    r.create_batch(
        [1; 16],
        &[
            NewRecord {
                namespace: 1,
                id: 1,
                schema: node_schema(),
                values: vec![Some(Value::Utf8("task".into()))],
            },
            NewRecord {
                namespace: 1,
                id: 2,
                schema: node_schema(),
                values: vec![Some(Value::Utf8("action".into()))],
            },
            NewRecord {
                namespace: 1,
                id: 3,
                schema: node_schema(),
                values: vec![Some(Value::Utf8("file".into()))],
            },
            NewRecord {
                namespace: 2,
                id: 4,
                schema: node_schema(),
                values: vec![Some(Value::Utf8("hidden".into()))],
            },
        ],
    )
    .unwrap();
    (dir, r)
}

fn authorized_bidirectional_navigation_rebuilds_and_snapshots_do_not_change(layout: GraphLayout) {
    let (dir, mut r) = setup();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(10, reference(1, 1), reference(1, 2)),
        0,
    )
    .unwrap();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(11, reference(1, 2), reference(1, 3)),
        0,
    )
    .unwrap();
    let old = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    assert_eq!(
        old.outgoing(&subject(&[1]), reference(1, 1), None, 100)
            .unwrap()
            .edges[0]
            .target,
        reference(1, 2)
    );
    assert_eq!(
        old.incoming(&subject(&[1]), reference(1, 3), Some(edge_schema()), 100)
            .unwrap()
            .edges[0]
            .source,
        reference(1, 2)
    );
    unlink(&mut r, &access(&[1], &[1]), reference(1, 10), 1).unwrap();
    assert_eq!(
        old.outgoing(&subject(&[1]), reference(1, 1), None, 100)
            .unwrap()
            .edges
            .len(),
        1
    );
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    let fresh = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    assert!(
        fresh
            .outgoing(&subject(&[1]), reference(1, 1), None, 100)
            .unwrap()
            .edges
            .is_empty()
    );
    assert_eq!(
        fresh
            .incoming(&subject(&[1]), reference(1, 3), None, 100)
            .unwrap()
            .edges
            .len(),
        1
    );
}

fn permissions_hide_edges_and_do_not_leak_has_more(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    link(
        &mut r,
        &access(&[1, 2], &[1]),
        edge(10, reference(1, 1), reference(2, 4)),
        0,
    )
    .unwrap();
    assert!(matches!(
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(11, reference(1, 1), reference(2, 4)),
            0
        ),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        unlink(&mut r, &access(&[1], &[1]), reference(1, 10), 1),
        Err(Error::Unauthorized)
    ));
    let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    let hidden = view
        .outgoing(&subject(&[1]), reference(1, 1), None, 1)
        .unwrap();
    assert!(hidden.edges.is_empty());
    assert!(!hidden.has_more);
    assert!(matches!(
        view.incoming(&subject(&[1]), reference(2, 4), None, 1),
        Err(Error::Unauthorized)
    ));
    assert!(matches!(
        view.outgoing(&subject(&[]), reference(1, 999), None, 1),
        Err(Error::Unauthorized)
    ));
}

fn cycles_and_limits_terminate_with_deterministic_truncation(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    for (id, a, b) in [(10, 1, 2), (11, 2, 3), (12, 3, 1)] {
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(id, reference(1, a), reference(1, b)),
            0,
        )
        .unwrap();
    }
    let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    let options = WalkOptions {
        direction: Direction::Outgoing,
        relation_schema: None,
        max_depth: 16,
        max_nodes: 10,
        max_edges: 10,
    };
    let result = view
        .walk(&subject(&[1]), reference(1, 1), &options)
        .unwrap();
    assert_eq!(
        result.nodes,
        vec![reference(1, 1), reference(1, 2), reference(1, 3)]
    );
    assert_eq!(result.edges.len(), 3);
    assert!(!result.truncated);
    let limited = view
        .walk(
            &subject(&[1]),
            reference(1, 1),
            &WalkOptions {
                max_nodes: 2,
                ..options
            },
        )
        .unwrap();
    assert_eq!(limited.nodes.len(), 2);
    assert!(limited.truncated);
    assert!(
        view.outgoing(&subject(&[1]), reference(1, 1), None, 0)
            .is_err()
    );
}

#[test]
fn write_requires_edge_grant_and_old_endpoint_visibility() {
    let (_dir, mut r) = setup();
    let record = edge(10, reference(1, 1), reference(1, 2));
    assert!(matches!(
        link(&mut r, &access(&[1], &[]), record.clone(), 0),
        Err(Error::Unauthorized)
    ));
    link(&mut r, &access(&[1], &[1]), record, 0).unwrap();
    assert!(matches!(
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(10, reference(1, 1), reference(2, 4)),
            1
        ),
        Err(Error::Unauthorized)
    ));
    link(
        &mut r,
        &access(&[1, 2], &[1]),
        edge(10, reference(1, 1), reference(2, 4)),
        1,
    )
    .unwrap();
    assert!(matches!(
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(10, reference(1, 1), reference(1, 2)),
            2
        ),
        Err(Error::Unauthorized)
    ));
    r.commit(
        [2; 16],
        &[
            Mutation::Delete {
                namespace: 1,
                id: 10,
                expected_revision: 2,
            },
            Mutation::Delete {
                namespace: 2,
                id: 4,
                expected_revision: 1,
            },
        ],
    )
    .unwrap();
}

fn type_filter_is_exact_and_visible_limit_does_not_count_hidden_edges(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    let other = SchemaRef {
        namespace: 2,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference: other,
        fields: vec![Field {
            label: "relation".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.register_relation_rule(RelationRule {
        schema: other,
        kind: 7,
        source_schema: node_schema(),
        target_schema: node_schema(),
    })
    .unwrap();
    link(
        &mut r,
        &access(&[1, 2], &[1]),
        edge(10, reference(1, 1), reference(2, 4)),
        0,
    )
    .unwrap();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(11, reference(1, 1), reference(1, 2)),
        0,
    )
    .unwrap();
    let mut alternate = edge(12, reference(1, 1), reference(1, 3));
    alternate.schema = other;
    link(&mut r, &access(&[1], &[1]), alternate, 0).unwrap();
    let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    let first = view
        .outgoing(&subject(&[1]), reference(1, 1), None, 1)
        .unwrap();
    assert_eq!(first.edges[0].reference, reference(1, 11));
    assert!(first.has_more);
    let filtered = view
        .outgoing(&subject(&[1]), reference(1, 1), Some(other), 1)
        .unwrap();
    assert_eq!(filtered.edges[0].reference, reference(1, 12));
    assert!(!filtered.has_more);
    assert!(
        view.outgoing(
            &subject(&[1]),
            reference(1, 1),
            Some(SchemaRef { id: 999, ..other }),
            1
        )
        .is_err()
    );
}

fn historical_view_can_be_rebuilt_after_atomic_detach_and_vertex_delete(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(10, reference(1, 1), reference(1, 2)),
        0,
    )
    .unwrap();
    let old = r.snapshot();
    r.commit(
        [2; 16],
        &[
            Mutation::Delete {
                namespace: 1,
                id: 2,
                expected_revision: 1,
            },
            Mutation::Delete {
                namespace: 1,
                id: 10,
                expected_revision: 1,
            },
        ],
    )
    .unwrap();
    let view = GraphView::new_with_layout(&r, old, layout).unwrap();
    assert_eq!(
        view.incoming(&subject(&[1]), reference(1, 2), None, 100)
            .unwrap()
            .edges
            .len(),
        1
    );
    let fresh = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    assert!(matches!(
        fresh.incoming(&subject(&[1]), reference(1, 2), None, 1),
        Err(Error::NotFound)
    ));
}

fn incoming_walk_is_bounded_and_legacy_relation_values_are_not_native_edges(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    for (id, a, b) in [(10, 1, 2), (11, 2, 3)] {
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(id, reference(1, a), reference(1, b)),
            0,
        )
        .unwrap();
    }
    let opaque = SchemaRef {
        namespace: 1,
        id: 3,
        version: 1,
    };
    r.register_schema(Schema {
        reference: opaque,
        fields: vec![Field {
            label: "opaque".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.create(
        1,
        99,
        opaque,
        &[Some(Value::Relation {
            source: reference(1, 999),
            kind: 999,
            target: reference(1, 998),
        })],
    )
    .unwrap();
    let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    let options = WalkOptions {
        direction: Direction::Incoming,
        relation_schema: None,
        max_depth: 16,
        max_nodes: 10,
        max_edges: 10,
    };
    let complete = view
        .walk(&subject(&[1]), reference(1, 3), &options)
        .unwrap();
    assert_eq!(
        complete.nodes,
        vec![reference(1, 3), reference(1, 2), reference(1, 1)]
    );
    assert_eq!(complete.edges.len(), 2);
    let depth = view
        .walk(
            &subject(&[1]),
            reference(1, 3),
            &WalkOptions {
                max_depth: 1,
                ..options.clone()
            },
        )
        .unwrap();
    assert_eq!(depth.nodes.len(), 2);
    assert_eq!(depth.edges.len(), 1);
    assert!(depth.truncated);
    let edges = view
        .walk(
            &subject(&[1]),
            reference(1, 3),
            &WalkOptions {
                max_edges: 1,
                ..options.clone()
            },
        )
        .unwrap();
    assert_eq!(edges.edges.len(), 1);
    assert!(edges.truncated);
    for bad in [
        WalkOptions {
            max_nodes: 0,
            ..options.clone()
        },
        WalkOptions {
            max_edges: 10_001,
            ..options.clone()
        },
        WalkOptions {
            max_depth: 17,
            ..options
        },
    ] {
        assert!(view.walk(&subject(&[1]), reference(1, 3), &bad).is_err());
    }
    assert!(
        view.outgoing(&subject(&[1]), reference(1, 99), None, 100)
            .unwrap()
            .edges
            .is_empty()
    );
}

fn self_loop_does_not_enqueue_the_same_node_or_report_false_truncation(layout: GraphLayout) {
    let (_dir, mut r) = setup();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(10, reference(1, 1), reference(1, 1)),
        0,
    )
    .unwrap();
    let view = GraphView::new_with_layout(&r, r.snapshot(), layout).unwrap();
    let walk = view
        .walk(
            &subject(&[1]),
            reference(1, 1),
            &WalkOptions {
                direction: Direction::Outgoing,
                relation_schema: Some(edge_schema()),
                max_depth: 1,
                max_nodes: 1,
                max_edges: 1,
            },
        )
        .unwrap();
    assert_eq!(walk.nodes, vec![reference(1, 1)]);
    assert_eq!(walk.edges.len(), 1);
    assert!(!walk.truncated);
}

#[test]
fn authorized_bidirectional_navigation_rebuilds_and_snapshots_do_not_change_legacy() {
    authorized_bidirectional_navigation_rebuilds_and_snapshots_do_not_change(GraphLayout::Legacy);
}
#[test]
fn authorized_bidirectional_navigation_rebuilds_and_snapshots_do_not_change_compact() {
    authorized_bidirectional_navigation_rebuilds_and_snapshots_do_not_change(GraphLayout::Compact);
}

#[test]
fn permissions_hide_edges_and_do_not_leak_has_more_legacy() {
    permissions_hide_edges_and_do_not_leak_has_more(GraphLayout::Legacy);
}
#[test]
fn permissions_hide_edges_and_do_not_leak_has_more_compact() {
    permissions_hide_edges_and_do_not_leak_has_more(GraphLayout::Compact);
}

#[test]
fn cycles_and_limits_terminate_with_deterministic_truncation_legacy() {
    cycles_and_limits_terminate_with_deterministic_truncation(GraphLayout::Legacy);
}
#[test]
fn cycles_and_limits_terminate_with_deterministic_truncation_compact() {
    cycles_and_limits_terminate_with_deterministic_truncation(GraphLayout::Compact);
}

#[test]
fn type_filter_is_exact_and_visible_limit_does_not_count_hidden_edges_legacy() {
    type_filter_is_exact_and_visible_limit_does_not_count_hidden_edges(GraphLayout::Legacy);
}
#[test]
fn type_filter_is_exact_and_visible_limit_does_not_count_hidden_edges_compact() {
    type_filter_is_exact_and_visible_limit_does_not_count_hidden_edges(GraphLayout::Compact);
}

#[test]
fn historical_view_can_be_rebuilt_after_atomic_detach_and_vertex_delete_legacy() {
    historical_view_can_be_rebuilt_after_atomic_detach_and_vertex_delete(GraphLayout::Legacy);
}
#[test]
fn historical_view_can_be_rebuilt_after_atomic_detach_and_vertex_delete_compact() {
    historical_view_can_be_rebuilt_after_atomic_detach_and_vertex_delete(GraphLayout::Compact);
}

#[test]
fn incoming_walk_is_bounded_and_legacy_relation_values_are_not_native_edges_legacy() {
    incoming_walk_is_bounded_and_legacy_relation_values_are_not_native_edges(GraphLayout::Legacy);
}
#[test]
fn incoming_walk_is_bounded_and_legacy_relation_values_are_not_native_edges_compact() {
    incoming_walk_is_bounded_and_legacy_relation_values_are_not_native_edges(GraphLayout::Compact);
}

#[test]
fn self_loop_does_not_enqueue_the_same_node_or_report_false_truncation_legacy() {
    self_loop_does_not_enqueue_the_same_node_or_report_false_truncation(GraphLayout::Legacy);
}
#[test]
fn self_loop_does_not_enqueue_the_same_node_or_report_false_truncation_compact() {
    self_loop_does_not_enqueue_the_same_node_or_report_false_truncation(GraphLayout::Compact);
}

#[test]
fn compact_matches_legacy_all_existing_scenarios() {
    let (_dir, mut r) = setup();
    for (id, a, b) in [(10, 1, 2), (11, 2, 3), (12, 3, 1), (13, 1, 1), (14, 1, 2)] {
        link(
            &mut r,
            &access(&[1], &[1]),
            edge(id, reference(1, a), reference(1, b)),
            0,
        )
        .unwrap();
    }
    let legacy = GraphView::new(&r, r.snapshot()).unwrap();
    let compact = GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact).unwrap();
    assert_eq!(compact.layout(), GraphLayout::Compact);
    for node in [
        reference(1, 1),
        reference(1, 2),
        reference(1, 3),
        reference(1, 10),
        reference(2, 4),
    ] {
        for grants in [subject(&[1]), subject(&[1, 2]), subject(&[])] {
            for filter in [None, Some(edge_schema()), Some(node_schema())] {
                for limit in [0, 1, 2, 1000, 1001] {
                    assert_eq!(
                        format!("{:?}", legacy.outgoing(&grants, node, filter, limit)),
                        format!("{:?}", compact.outgoing(&grants, node, filter, limit))
                    );
                    assert_eq!(
                        format!("{:?}", legacy.incoming(&grants, node, filter, limit)),
                        format!("{:?}", compact.incoming(&grants, node, filter, limit))
                    );
                }
                for direction in [Direction::Outgoing, Direction::Incoming] {
                    for (depth, nodes, edges) in [
                        (0, 1, 1),
                        (1, 1, 1),
                        (1, 10, 10),
                        (2, 2, 2),
                        (3, 10, 10),
                        (16, 10000, 10000),
                    ] {
                        let o = WalkOptions {
                            direction,
                            relation_schema: filter,
                            max_depth: depth,
                            max_nodes: nodes,
                            max_edges: edges,
                        };
                        assert_eq!(
                            format!("{:?}", legacy.walk(&grants, node, &o)),
                            format!("{:?}", compact.walk(&grants, node, &o))
                        );
                    }
                }
            }
        }
    }
    assert_eq!(
        compact
            .outgoing(&subject(&[1]), reference(1, 1), None, 10)
            .unwrap()
            .edges
            .iter()
            .map(|e| e.reference.id)
            .collect::<Vec<_>>(),
        vec![10, 13, 14]
    );
}

#[test]
fn compact_unique_metadata_exact_csr_and_rebuild_recovery_retention() {
    let (dir, mut r) = setup();
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(10, reference(1, 1), reference(1, 2)),
        0,
    )
    .unwrap();
    let old_snapshot = r.snapshot();
    let old = GraphView::new_with_layout(&r, old_snapshot.clone(), GraphLayout::Compact).unwrap();
    let c = old.compact.as_ref().unwrap();
    assert_eq!(c.nodes.len(), 5); // four facts and the edge identity
    assert_eq!(c.edges.len(), 1);
    assert!(
        old.nodes.is_empty()
            && old.edges.is_empty()
            && old.sources.is_empty()
            && old.targets.is_empty()
    );
    for csr in [&c.sources, &c.targets] {
        assert_eq!(csr.offsets.len(), c.nodes.len() + 1);
        assert_eq!(csr.postings, vec![0]);
        assert_eq!(*csr.offsets.last().unwrap(), 1);
        assert!(csr.offsets.windows(2).all(|w| w[0] <= w[1]));
    }
    let migrated = SchemaRef {
        version: 2,
        ..node_schema()
    };
    r.register_schema(Schema {
        reference: migrated,
        fields: vec![Field {
            label: "name".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    assert!(matches!(
        r.update(
            NewRecord {
                namespace: 1,
                id: 1,
                schema: migrated,
                values: vec![Some(Value::Utf8("new".into()))]
            },
            1
        ),
        Err(Error::Conflict)
    ));
    link(
        &mut r,
        &access(&[1], &[1]),
        edge(10, reference(1, 2), reference(1, 3)),
        1,
    )
    .unwrap();
    let fresh = GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact).unwrap();
    assert!(
        fresh
            .outgoing(&subject(&[1]), reference(1, 1), None, 10)
            .unwrap()
            .edges
            .is_empty()
    );
    assert_eq!(
        old.outgoing(&subject(&[1]), reference(1, 1), None, 10)
            .unwrap()
            .edges
            .len(),
        1
    );
    r.compact(canonical_registry::Retention::KeepLast(1))
        .unwrap();
    assert!(GraphView::new_with_layout(&r, old_snapshot.clone(), GraphLayout::Compact).is_ok());
    let mut invalid_snapshot = old_snapshot;
    invalid_snapshot.commit_sequence = 0;
    assert!(GraphView::new_with_layout(&r, invalid_snapshot, GraphLayout::Compact).is_err());
    for layout in [GraphLayout::Legacy, GraphLayout::Compact] {
        assert_eq!(
            GraphView::new_with_layout(&r, r.snapshot(), layout)
                .unwrap()
                .outgoing(&subject(&[1]), reference(1, 2), None, 10)
                .unwrap()
                .edges[0]
                .revision,
            2
        );
    }
    drop(r);
    let (mut r, _) = Registry::recover(dir.path()).unwrap();
    assert_eq!(
        GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact)
            .unwrap()
            .incoming(&subject(&[1]), reference(1, 3), None, 10)
            .unwrap()
            .edges
            .len(),
        1
    );
    unlink(&mut r, &access(&[1], &[1]), reference(1, 10), 2).unwrap();
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert!(
        GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact)
            .unwrap()
            .outgoing(&subject(&[1]), reference(1, 2), None, 10)
            .unwrap()
            .edges
            .is_empty()
    );
}

#[test]
fn compact_empty_and_namespace_zero_sparse_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    let empty = GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact).unwrap();
    let c = empty.compact.as_ref().unwrap();
    assert!(c.nodes.is_empty() && c.edges.is_empty());
    assert_eq!(c.sources.offsets, vec![0]);
    assert_eq!(c.targets.offsets, vec![0]);
    assert!(matches!(
        empty.outgoing(
            &subject(&[]),
            reference(0, u64::MAX),
            Some(node_schema()),
            0
        ),
        Err(Error::Unauthorized)
    ));
    r.register_namespace(Namespace {
        id: 0,
        name: "zero".into(),
        level: Level::Local,
    })
    .unwrap();
    let sr = SchemaRef {
        namespace: 0,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: vec![],
    })
    .unwrap();
    r.create(0, u64::MAX, sr, &[]).unwrap();
    let compact = GraphView::new_with_layout(&r, r.snapshot(), GraphLayout::Compact).unwrap();
    assert!(
        compact
            .outgoing(&subject(&[0]), reference(0, u64::MAX), None, 1)
            .unwrap()
            .edges
            .is_empty()
    );
    assert!(matches!(
        compact.outgoing(&subject(&[0]), reference(0, 1), None, 1),
        Err(Error::NotFound)
    ));
}

#[test]
fn compact_capacity_overflow_is_rejected_without_allocation() {
    assert!(matches!(
        check_capacity::<NativeEdge>(usize::MAX),
        Err(Error::TooLarge)
    ));
    assert!(matches!(
        check_capacity::<usize>(isize::MAX as usize),
        Err(Error::TooLarge)
    ));
    assert!(check_capacity::<usize>(0).is_ok());
}
