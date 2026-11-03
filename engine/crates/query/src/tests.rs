use super::*;
use canonical_registry::{Field, Level, Namespace, NewRecord};

#[test]
fn native_relation_visibility_is_snapshot_local_and_precedes_counts_and_paging() {
    use canonical_registry::{Reference, RelationRule};
    let (_, mut registry, _) = setup();
    let edge_schema = SchemaRef {
        id: 2,
        ..reference()
    };
    registry
        .register_schema(Schema {
            reference: edge_schema,
            fields: vec![Field {
                label: "edge".into(),
                kind: FieldType::Relation,
                optional: false,
            }],
        })
        .unwrap();
    registry
        .register_relation_rule(RelationRule {
            schema: edge_schema,
            kind: 10,
            source_schema: reference(),
            target_schema: reference(),
        })
        .unwrap();
    let edge = |id, target_namespace, target_id| NewRecord {
        namespace: 1,
        id,
        schema: edge_schema,
        values: vec![Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 10,
            target: Reference {
                namespace: target_namespace,
                id: target_id,
            },
        })],
    };
    registry
        .create(1, 100, edge_schema, &edge(100, 1, 2).values)
        .unwrap();
    let old_snapshot = registry.snapshot();
    registry.update(edge(100, 2, 5), 1).unwrap();
    registry
        .create(1, 102, edge_schema, &edge(102, 1, 2).values)
        .unwrap();
    let mut old = QueryEngine::new(&registry, old_snapshot).unwrap();
    let mut current = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    let mut request = query();
    request.schemas = vec![edge_schema];
    request.limit = 1;
    let past = old.query(&subject(), &request).unwrap();
    assert_eq!(
        past.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![100]
    );
    let page = current.query(&subject(), &request).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![102]
    );
    assert_eq!(page.plan.candidates, 1);
    assert_eq!(page.plan.rows_examined, 1);
    assert!(page.next.is_none());
    let broad = Subject {
        id: "broad".into(),
        readable_namespaces: [1, 2].into_iter().collect(),
    };
    assert_eq!(current.query(&broad, &request).unwrap().records[0].id, 100);
}

#[test]
fn unbound_legacy_relation_values_keep_record_namespace_visibility() {
    use canonical_registry::Reference;
    let (_, mut registry, _) = setup();
    let opaque = SchemaRef {
        id: 3,
        ..reference()
    };
    registry
        .register_schema(Schema {
            reference: opaque,
            fields: vec![Field {
                label: "opaque".into(),
                kind: FieldType::Relation,
                optional: false,
            }],
        })
        .unwrap();
    registry
        .create(
            1,
            101,
            opaque,
            &[Some(Value::Relation {
                source: Reference {
                    namespace: 1,
                    id: 1,
                },
                kind: 77,
                target: Reference {
                    namespace: 2,
                    id: 5,
                },
            })],
        )
        .unwrap();
    let mut engine = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    let mut request = query();
    request.schemas = vec![opaque];
    assert_eq!(
        engine.query(&subject(), &request).unwrap().records[0].id,
        101
    );
}

fn reference() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn field() -> FieldRef {
    FieldRef {
        schema: reference(),
        position: 0,
    }
}
fn setup() -> (tempfile::TempDir, Registry, QueryEngine) {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = Registry::open(dir.path()).unwrap();
    for id in [1, 2] {
        registry
            .register_namespace(Namespace {
                id,
                name: format!("ns{id}"),
                level: Level::Local,
            })
            .unwrap();
    }
    registry
        .register_schema(Schema {
            reference: reference(),
            fields: vec![Field {
                label: "任意".into(),
                kind: FieldType::UInt,
                optional: true,
            }],
        })
        .unwrap();
    let records: Vec<_> = (1..=6)
        .map(|id| NewRecord {
            namespace: if id <= 4 { 1 } else { 2 },
            id,
            schema: reference(),
            values: vec![if id == 1 {
                None
            } else {
                Some(Value::UInt(id % 3))
            }],
        })
        .collect();
    registry.create_batch([1; 16], &records).unwrap();
    let engine = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    (dir, registry, engine)
}
fn subject() -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: [1].into_iter().collect(),
    }
}
fn query() -> Query {
    Query {
        namespaces: vec![1],
        schemas: vec![],
        filter: Predicate::True,
        order: Order::default(),
        limit: 100,
        cursor: None,
    }
}

#[test]
fn indexed_predicates_and_stable_order_match_expected_rows() {
    let (_, _, mut engine) = setup();
    let mut request = query();
    request.filter = Predicate::Range {
        field: field(),
        lower: Some(Value::UInt(1)),
        upper: Some(Value::UInt(2)),
    };
    request.order = Order {
        field: Some(field()),
        descending: false,
    };
    let page = engine.query(&subject(), &request).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![4, 2]
    );
    assert_eq!(page.plan.strategy, PlanStrategy::Indexed);
    assert_eq!(page.plan.rows_examined, 2);
    request.filter = Predicate::Or(vec![
        Predicate::Eq(field(), Value::UInt(0)),
        Predicate::Eq(field(), Value::UInt(1)),
    ]);
    assert_eq!(
        engine
            .query(&subject(), &request)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
}

#[test]
fn deny_first_type_limits_and_cursor_context_are_enforced() {
    let (_, _, mut engine) = setup();
    let mut request = query();
    request.limit = 1;
    let page = engine.query(&subject(), &request).unwrap();
    let token = page.next.unwrap();
    request.cursor = Some(token.clone());
    assert_eq!(engine.query(&subject(), &request).unwrap().records[0].id, 2);
    let mut invalid = request.clone();
    invalid.filter = Predicate::Eq(field(), Value::UInt(2));
    assert!(matches!(
        engine.query(&subject(), &invalid),
        Err(Error::InvalidCursor)
    ));
    let mut bad = token.to_bytes();
    bad[0] ^= 1;
    request.cursor = Some(Cursor::from_bytes(bad));
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    request.namespaces = vec![2];
    request.filter = Predicate::Eq(field(), Value::Bool(true));
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::Unauthorized)
    ));
    request = query();
    request.filter = Predicate::Eq(field(), Value::Bool(true));
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::InvalidInput(_))
    ));
    request = query();
    request.limit = 0;
    assert!(engine.query(&subject(), &request).is_err());
}

#[test]
fn snapshot_and_cursor_owner_do_not_follow_later_commits() {
    let (_, mut registry, mut engine) = setup();
    let mut request = query();
    request.limit = 2;
    let first = engine.query(&subject(), &request).unwrap();
    let cursor = first.next.unwrap();
    registry
        .create(1, 99, reference(), &[Some(Value::UInt(1))])
        .unwrap();
    let mut other = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    request.cursor = Some(cursor);
    assert!(matches!(
        other.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    let page = engine.query(&subject(), &request).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert!(page.next.is_none());
}

#[test]
fn schema_filter_uses_derived_schema_index() {
    let (_, mut registry, _) = setup();
    let schema = SchemaRef {
        version: 2,
        ..reference()
    };
    registry
        .register_schema(Schema {
            reference: schema,
            fields: vec![],
        })
        .unwrap();
    registry.create(1, 99, schema, &[]).unwrap();
    let mut engine = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    let mut request = query();
    request.schemas = vec![schema];
    let page = engine.query(&subject(), &request).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![99]
    );
    assert_eq!(page.plan.strategy, PlanStrategy::Indexed);
    assert_eq!(page.plan.index_lookups, 1);
    assert_eq!(page.plan.rows_examined, 1);
}

#[test]
fn query_budgets_empty_boolean_sets_and_cursor_window_are_explicit() {
    let (_, _, mut engine) = setup();
    let mut request = query();
    request.filter = Predicate::And(vec![]);
    assert_eq!(engine.query(&subject(), &request).unwrap().records.len(), 4);
    request.filter = Predicate::Or(vec![]);
    assert!(
        engine
            .query(&subject(), &request)
            .unwrap()
            .records
            .is_empty()
    );
    let mut deep = Predicate::True;
    for _ in 0..MAX_QUERY_DEPTH {
        deep = Predicate::And(vec![deep]);
    }
    request.filter = deep;
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::TooLarge)
    ));
    request.filter = Predicate::And(vec![Predicate::True; MAX_QUERY_NODES]);
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::TooLarge)
    ));
    request = query();
    request.filter = Predicate::Range {
        field: field(),
        lower: Some(Value::UInt(2)),
        upper: Some(Value::UInt(1)),
    };
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::InvalidInput(_))
    ));
    request = query();
    request.limit = 1;
    let expired = engine.query(&subject(), &request).unwrap().next.unwrap();
    for _ in 0..MAX_CURSORS {
        engine.query(&subject(), &request).unwrap();
    }
    request.cursor = Some(expired);
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
}

#[test]
fn changing_subject_identity_scopes_or_order_invalidates_cursor() {
    let (_, _, mut engine) = setup();
    let mut request = query();
    request.limit = 1;
    let cursor = engine.query(&subject(), &request).unwrap().next.unwrap();
    request.cursor = Some(cursor.clone());
    let other = Subject {
        id: "other".into(),
        ..subject()
    };
    assert!(matches!(
        engine.query(&other, &request),
        Err(Error::InvalidCursor)
    ));
    let changed = Subject {
        readable_namespaces: [1, 2].into_iter().collect(),
        ..subject()
    };
    assert!(matches!(
        engine.query(&changed, &request),
        Err(Error::InvalidCursor)
    ));
    request.order.descending = true;
    assert!(matches!(
        engine.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    request = query();
    request.order.descending = true;
    assert_eq!(
        engine
            .query(&subject(), &request)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![4, 3, 2, 1]
    );
}

#[test]
fn owner_is_not_reused_when_slot_one_exists_in_another_engine() {
    let (_, registry, mut first) = setup();
    let mut request = query();
    request.limit = 1;
    let old = first.query(&subject(), &request).unwrap().next.unwrap();
    assert_eq!(old.to_bytes().len(), 24);
    let mut other = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    let fresh = other.query(&subject(), &request).unwrap().next.unwrap();
    assert_eq!(&old.to_bytes()[16..], &fresh.to_bytes()[16..]); // both issued slot1
    assert_ne!(&old.to_bytes()[..16], &fresh.to_bytes()[..16]); // public random128-bit owner
    request.cursor = Some(Cursor::from_bytes(old.to_bytes()));
    assert!(matches!(
        other.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
}

#[test]
fn entropy_failure_is_an_error_without_counter_fallback() {
    let result = session_owner(|_| Err(getrandom::Error::UNSUPPORTED));
    assert!(matches!(
        result,
        Err(Error::InvalidInput("query session entropy unavailable"))
    ));
}

#[test]
fn query_rows_share_snapshot_payload_allocation_instead_of_cloning_records() {
    let (_, registry, _) = setup();
    let snapshot = registry.snapshot();
    let pointer = snapshot.record(1, 2).unwrap().payload.as_ptr();
    let engine = QueryEngine::new(&registry, snapshot.clone()).unwrap();
    assert_eq!(
        engine.snapshot.record(1, 2).unwrap().payload.as_ptr(),
        pointer
    );
    assert_eq!(
        engine
            .rows
            .iter()
            .find(|entry| entry.key == (1, 2, 1))
            .unwrap()
            .row
            .schema,
        snapshot.record(1, 2).unwrap().schema
    );
    assert_eq!(engine.snapshot.len(), snapshot.len());
}

#[test]
fn borrowed_selection_matches_legacy_counts_order_and_all_pages() {
    let (_dir, mut registry, _) = setup();
    let v2 = SchemaRef {
        version: 2,
        ..reference()
    };
    registry
        .register_schema(Schema {
            reference: v2,
            fields: vec![Field {
                label: "v".into(),
                kind: FieldType::UInt,
                optional: true,
            }],
        })
        .unwrap();
    for id in 7..=10 {
        registry
            .create(
                if id % 2 == 0 { 1 } else { 2 },
                id,
                v2,
                &[Some(Value::UInt(id % 3))],
            )
            .unwrap();
    }
    let mut engine = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    let eq = Predicate::Eq(field(), Value::UInt(1));
    let absent = Predicate::Eq(field(), Value::UInt(99));
    let range = Predicate::Range {
        field: field(),
        lower: Some(Value::UInt(0)),
        upper: Some(Value::UInt(1)),
    };
    let eq2 = Predicate::Eq(
        FieldRef {
            schema: v2,
            position: 0,
        },
        Value::UInt(1),
    );
    let predicates = vec![
        Predicate::True,
        eq.clone(),
        absent.clone(),
        range.clone(),
        Predicate::And(vec![]),
        Predicate::Or(vec![]),
        Predicate::And(vec![eq.clone(), Predicate::True]),
        Predicate::And(vec![
            eq.clone(),
            Predicate::And(vec![eq.clone(), range.clone()]),
        ]),
        Predicate::And(vec![eq.clone(), absent.clone()]),
        Predicate::And(vec![eq.clone(), eq2.clone()]),
        Predicate::And(vec![
            eq.clone(),
            Predicate::Or(vec![range.clone(), Predicate::True]),
        ]),
        Predicate::And(vec![range.clone(), Predicate::True]),
        Predicate::And(vec![
            Predicate::Or(vec![eq.clone(), eq2.clone()]),
            range.clone(),
        ]),
        // Same topology as filter_request: version-specific conjunctions under OR.
        Predicate::Or(vec![
            Predicate::And(vec![eq.clone(), eq.clone()]),
            Predicate::And(vec![eq2.clone(), eq2.clone()]),
        ]),
        Predicate::Or(vec![
            Predicate::And(vec![eq.clone(), Predicate::True]),
            Predicate::And(vec![eq.clone(), range.clone()]),
        ]),
        Predicate::Or(vec![
            Predicate::And(vec![
                eq.clone(),
                Predicate::Range {
                    field: field(),
                    lower: Some(Value::UInt(2)),
                    upper: Some(Value::UInt(2)),
                },
            ]),
            Predicate::And(vec![eq2.clone(), Predicate::True]),
        ]),
        Predicate::Or(vec![
            Predicate::And(vec![range.clone(), Predicate::True]),
            eq.clone(),
        ]),
        Predicate::Or(vec![
            Predicate::And(vec![eq.clone(), Predicate::True]),
            Predicate::True,
            eq2.clone(),
        ]),
        Predicate::Or(vec![
            Predicate::Or(vec![eq.clone(), eq2.clone()]),
            Predicate::And(vec![eq.clone(), range.clone()]),
        ]),
        Predicate::Or(vec![eq, eq2]),
        Predicate::Or(vec![absent, Predicate::True]),
    ];
    for predicate in predicates {
        for schemas in [
            vec![],
            vec![reference()],
            vec![v2],
            vec![reference(), v2, reference()],
        ] {
            for namespaces in [vec![1], vec![1, 2], vec![2, 1, 2]] {
                let subject = Subject {
                    id: "parity".into(),
                    readable_namespaces: namespaces.iter().copied().collect(),
                };
                for order in [
                    Order::default(),
                    Order {
                        field: None,
                        descending: true,
                    },
                    Order {
                        field: Some(field()),
                        descending: false,
                    },
                    Order {
                        field: Some(field()),
                        descending: true,
                    },
                ] {
                    let mut request = Query {
                        namespaces: namespaces.clone(),
                        schemas: schemas.clone(),
                        filter: predicate.clone(),
                        order,
                        limit: 2,
                        cursor: None,
                    };
                    let old = engine.legacy_selection_keys(&subject, &request).unwrap();
                    let new = engine.selection_keys(&subject, &request).unwrap();
                    assert_eq!(old, new, "predicate {:?}", request.filter);
                    let (mut expected, examined, probes, indexed) = old;
                    expected.sort_by(|a, b| engine.compare(*a, *b, &request.order));
                    let plan = QueryPlan {
                        strategy: if indexed {
                            PlanStrategy::Indexed
                        } else {
                            PlanStrategy::Scan
                        },
                        index_lookups: probes,
                        candidates: examined,
                        rows_examined: examined,
                        snapshot_sequence: engine.sequence,
                    };
                    let mut all = vec![];
                    loop {
                        let page = engine.query_ids(&subject, &request).unwrap();
                        assert_eq!(page.plan, plan);
                        all.extend(page.ids.iter().map(|id| (id.namespace, id.id, id.revision)));
                        if page.next.is_none() {
                            break;
                        }
                        request.cursor = page.next;
                    }
                    assert_eq!(all, expected);
                    request.cursor = None;
                    let full = engine.query(&subject, &request).unwrap();
                    assert_eq!(full.plan, plan);
                    let expected_records = expected
                        .iter()
                        .take(2)
                        .map(|key| engine.snapshot.record(key.0, key.1).unwrap().clone())
                        .collect::<Vec<_>>();
                    assert_eq!(full.records, expected_records);
                    let projection = BTreeMap::from([(reference(), vec![0]), (v2, vec![0])]);
                    let projected = engine
                        .query_projected(&subject, &request, &projection)
                        .unwrap();
                    assert_eq!(projected.plan, plan);
                    assert_eq!(projected.next.is_some(), expected.len() > 2);
                    for (actual, record) in projected.rows.iter().zip(&expected_records) {
                        assert_eq!(actual.identity.id, record.id);
                        assert_eq!(actual.identity.namespace, record.namespace);
                        assert_eq!(
                            actual.fields,
                            registry
                                .decode_values(record.schema, &record.payload)
                                .unwrap()
                        );
                    }
                    assert_eq!(projected.rows.len(), expected_records.len());
                }
            }
        }
    }
}

#[test]
fn production_intersection_reduces_real_comparisons_and_keeps_skewed_lookup_bounded() {
    use std::cell::Cell;
    thread_local! {static KEY_COMPARISONS:Cell<usize>=const{Cell::new(0)};}
    #[derive(Clone)]
    struct Counted {
        key: u64,
    }
    impl PartialEq for Counted {
        fn eq(&self, other: &Self) -> bool {
            self.key == other.key
        }
    }
    impl Eq for Counted {}
    impl PartialOrd for Counted {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Counted {
        fn cmp(&self, other: &Self) -> Ordering {
            KEY_COMPARISONS.with(|cost| cost.set(cost.get() + 1));
            self.key.cmp(&other.key)
        }
    }
    let fixture = |a: Vec<u64>, b: Vec<u64>, bound: usize| {
        let build = |keys: &[u64]| {
            keys.iter()
                .map(|&key| Counted { key })
                .collect::<BTreeSet<_>>()
        };
        let left = build(&a);
        let right = build(&b);
        let expected = a
            .iter()
            .copied()
            .filter(|key| b.binary_search(key).is_ok())
            .collect::<Vec<_>>();
        let left = left.into_iter().collect::<Vec<_>>();
        let right = right.into_iter().collect::<Vec<_>>();
        for (a, b) in [(&left, &right), (&right, &left)] {
            KEY_COMPARISONS.with(|cost| cost.set(0));
            let result = intersect_slices(a, b)
                .map(|key| key.key)
                .collect::<Vec<_>>();
            let comparisons = KEY_COMPARISONS.with(Cell::get);
            assert_eq!(result, expected);
            eprintln!(
                "posting lengths={}/{} comparisons={comparisons} matches={}",
                a.len(),
                b.len(),
                result.len()
            );
            assert!(
                comparisons <= bound,
                "lengths{}/{} performed {comparisons} comparisons, bound{bound}",
                a.len(),
                b.len()
            );
        }
    };
    let equal = (0..4096).collect::<Vec<_>>();
    fixture(equal.clone(), equal, 3 * 4096);
    fixture(vec![8191], (0..16384).collect(), 128);
    fixture(
        (0..4096).collect(),
        (0..4095).chain(std::iter::once(10000)).collect(),
        3 * 4096,
    );
    fixture(
        (0..2048).map(|i| i * 2).collect(),
        (0..2048).map(|i| i * 2 + 1).collect(),
        3 * 4096,
    );
    fixture((0..1024).collect(), (2048..3072).collect(), 32);
    fixture((1024..2048).collect(), (0..1024).collect(), 32);
    fixture(vec![], (0..1024).collect(), 8);
}

#[test]
fn additional_equalities_after_first_pair_preserve_exact_candidates_and_probes() {
    let (_dir, _registry, mut engine) = setup();
    let eq = |value| Predicate::Eq(field(), Value::UInt(value));
    for conjunction in [
        Predicate::And(vec![eq(1), eq(1), eq(1)]),
        Predicate::And(vec![eq(1), eq(1), eq(0)]),
        Predicate::And(vec![eq(1), eq(99), eq(1)]),
        Predicate::And(vec![eq(1), Predicate::And(vec![eq(1), eq(1)])]),
    ] {
        for filter in [
            conjunction.clone(),
            Predicate::Or(vec![conjunction, Predicate::And(vec![eq(0), eq(0), eq(0)])]),
        ] {
            let mut request = query();
            request.filter = filter;
            let old = engine.legacy_selection_keys(&subject(), &request).unwrap();
            assert_eq!(engine.selection_keys(&subject(), &request).unwrap(), old);
            let page = engine.query_ids(&subject(), &request).unwrap();
            assert_eq!(
                page.ids
                    .iter()
                    .map(|r| (r.namespace, r.id, r.revision))
                    .collect::<Vec<_>>(),
                old.0
            );
            assert_eq!(page.plan.index_lookups, old.2);
            assert_eq!(page.plan.candidates, old.1);
        }
    }
}

#[test]
fn ordinal_indices_and_all_scalar_ranges_match_independent_legacy_and_canonical_rows() {
    use canonical_registry::Reference;
    let (_dir, mut registry, _) = setup();
    let sr = SchemaRef {
        id: 77,
        ..reference()
    };
    let kinds = [
        FieldType::UInt,
        FieldType::Int,
        FieldType::FixedInt,
        FieldType::Decimal { scale: 2 },
        FieldType::Float,
        FieldType::Enum {
            variants: vec![1, 2, 3],
        },
        FieldType::Bool,
        FieldType::Ref,
        FieldType::Utf8,
        FieldType::Bytes,
    ];
    registry
        .register_schema(Schema {
            reference: sr,
            fields: kinds
                .iter()
                .enumerate()
                .map(|(i, kind)| Field {
                    label: format!("f{i}"),
                    kind: kind.clone(),
                    optional: i == 0,
                })
                .collect(),
        })
        .unwrap();
    let mut literals = vec![vec![]; kinds.len()];
    let records = (0..17)
        .map(|i| {
            let rank = (i * 7) % 17;
            let signed = rank as i64 - 8;
            let values = vec![
                if rank == 0 {
                    None
                } else {
                    Some(Value::UInt(rank))
                },
                Some(Value::Int(signed)),
                Some(Value::FixedInt(signed)),
                Some(Value::Decimal {
                    mantissa: signed * 100,
                    scale: 2,
                }),
                Some(Value::Float(if rank == 8 {
                    -0.0
                } else if rank == 9 {
                    0.0
                } else {
                    signed as f64
                })),
                Some(Value::Enum(1 + (rank % 3) as u32)),
                Some(Value::Bool(rank % 2 == 0)),
                Some(Value::Ref(Reference {
                    namespace: 1,
                    id: 1 + rank % 4,
                })),
                Some(Value::Utf8(format!("s{rank:02}é日本"))),
                Some(Value::Bytes(vec![rank as u8, 0, 255])),
            ];
            for (pos, value) in values.iter().enumerate() {
                if let Some(value) = value {
                    literals[pos].push(value.clone());
                }
            }
            NewRecord {
                namespace: 1 + i % 2,
                id: 100 + i,
                schema: sr,
                values,
            }
        })
        .collect::<Vec<_>>();
    registry.create_batch([0x77; 16], &records).unwrap();
    let mut engine = QueryEngine::new(&registry, registry.snapshot()).unwrap();
    assert!(engine.rows.windows(2).all(|w| w[0].key < w[1].key));
    for (field, keys) in &engine.secondary {
        assert!(
            keys.iter()
                .all(|&o| o < engine.rows.len() && engine.rows[o].row.schema == field.schema)
        );
        assert!(keys.windows(2).all(|w| {
            let a = engine.posting_value(w[0], *field);
            let b = engine.posting_value(w[1], *field);
            a < b || (a == b && w[0] < w[1])
        }));
    }
    for keys in engine
        .by_namespace
        .values()
        .chain(engine.by_schema.values())
    {
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }
    for (pos, mut values) in literals.into_iter().enumerate() {
        values.sort_by_key(|v| scalar(&kinds[pos], v).unwrap());
        values.dedup_by(|a, b| scalar(&kinds[pos], a).unwrap() == scalar(&kinds[pos], b).unwrap());
        let field = FieldRef {
            schema: sr,
            position: pos,
        };
        let mid = values[values.len() / 2].clone();
        let mut predicates = values
            .iter()
            .cloned()
            .map(|v| Predicate::Eq(field, v))
            .collect::<Vec<_>>();
        predicates.extend([
            Predicate::Range {
                field,
                lower: Some(values[0].clone()),
                upper: Some(values.last().unwrap().clone()),
            },
            Predicate::Range {
                field,
                lower: Some(mid.clone()),
                upper: None,
            },
            Predicate::Range {
                field,
                lower: None,
                upper: Some(mid.clone()),
            },
            Predicate::Range {
                field,
                lower: Some(mid.clone()),
                upper: Some(mid),
            },
        ]);
        for predicate in predicates {
            for order in [
                Order::default(),
                Order {
                    field: None,
                    descending: true,
                },
                Order {
                    field: Some(field),
                    descending: false,
                },
                Order {
                    field: Some(field),
                    descending: true,
                },
            ] {
                let mut request = Query {
                    namespaces: vec![1, 2],
                    schemas: vec![sr],
                    filter: predicate.clone(),
                    order,
                    limit: 3,
                    cursor: None,
                };
                let reader = Subject {
                    id: "scalar-range".into(),
                    readable_namespaces: BTreeSet::from([1, 2]),
                };
                let old = engine.legacy_selection_keys(&reader, &request).unwrap();
                assert_eq!(engine.selection_keys(&reader, &request).unwrap(), old);
                let mut expected = old.0;
                expected.sort_by(|a, b| engine.compare(*a, *b, &request.order));
                let mut actual = vec![];
                loop {
                    let page = engine.query_ids(&reader, &request).unwrap();
                    assert_eq!(page.plan.index_lookups, old.2);
                    assert_eq!(page.plan.candidates, old.1);
                    actual.extend(page.ids.iter().map(|r| (r.namespace, r.id, r.revision)));
                    if page.next.is_none() {
                        break;
                    }
                    request.cursor = page.next;
                }
                assert_eq!(actual, expected);
            }
        }
    }
}
