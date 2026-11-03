use canonical_core::Error;
use canonical_query::*;
use canonical_registry::*;
use std::collections::{BTreeMap, BTreeSet};
fn sr(version: u32) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version,
    }
}
fn child() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    }
}
fn subject() -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    }
}
fn request() -> Query {
    Query {
        namespaces: vec![1],
        schemas: vec![sr(1), sr(2)],
        filter: Predicate::True,
        order: Order::default(),
        limit: 1000,
        cursor: None,
    }
}
fn setup() -> (tempfile::TempDir, Registry, QueryEngine) {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("ns{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    r.register_schema(Schema {
        reference: child(),
        fields: vec![Field {
            label: "flag".into(),
            kind: FieldType::Bool,
            optional: false,
        }],
    })
    .unwrap();
    let fields = vec![
        Field {
            label: "action".into(),
            kind: FieldType::UInt,
            optional: false,
        },
        Field {
            label: "large".into(),
            kind: FieldType::Utf8,
            optional: false,
        },
        Field {
            label: "maybe".into(),
            kind: FieldType::UInt,
            optional: true,
        },
        Field {
            label: "array".into(),
            kind: FieldType::Array {
                element: ScalarType::Utf8,
                max_items: 4,
            },
            optional: false,
        },
        Field {
            label: "child".into(),
            kind: FieldType::Struct(child()),
            optional: false,
        },
        Field {
            label: "ref".into(),
            kind: FieldType::Ref,
            optional: false,
        },
        Field {
            label: "opaque_relation".into(),
            kind: FieldType::Relation,
            optional: false,
        },
    ];
    r.register_schema(Schema {
        reference: sr(1),
        fields: fields.clone(),
    })
    .unwrap();
    let mut v2 = fields;
    v2.push(Field {
        label: "extra".into(),
        kind: FieldType::UInt,
        optional: true,
    });
    r.register_schema(Schema {
        reference: sr(2),
        fields: v2,
    })
    .unwrap();
    let nested = r
        .encode_values(child(), &[Some(Value::Bool(true))])
        .unwrap();
    for (ns, id, version, maybe) in [
        (1, 1, 1, None),
        (1, 2, 2, Some(9)),
        (1, 3, 1, Some(3)),
        (2, 9, 1, None),
    ] {
        let mut values = vec![
            Some(Value::UInt(id)),
            Some(Value::Utf8("texto; ".repeat(1024))),
            maybe.map(Value::UInt),
            Some(Value::Array(vec![ScalarValue::Utf8("日本語".into())])),
            Some(Value::Struct(nested.clone())),
            Some(Value::Ref(Reference {
                namespace: 1,
                id: 1,
            })),
            Some(Value::Relation {
                source: Reference {
                    namespace: 1,
                    id: 1,
                },
                kind: 7,
                target: Reference {
                    namespace: 1,
                    id: 3,
                },
            }),
        ];
        if version == 2 {
            values.push(None);
        }
        r.create(ns, id, sr(version), &values).unwrap();
    }
    let q = QueryEngine::new(&r, r.snapshot()).unwrap();
    (d, r, q)
}
#[test]
fn ids_match_full_records_zero_one_many_and_pages_without_payload_output() {
    let (_directory, _, mut q) = setup();
    let mut spec = request();
    spec.limit = 1;
    let mut expected = Vec::new();
    loop {
        let p = q.query(&subject(), &spec).unwrap();
        expected.extend(
            p.records
                .into_iter()
                .map(|r| (r.namespace, r.id, r.revision, r.schema)),
        );
        spec.cursor = p.next;
        if spec.cursor.is_none() {
            break;
        }
    }
    spec.cursor = None;
    let mut actual = Vec::new();
    loop {
        let p = q.query_ids(&subject(), &spec).unwrap();
        actual.extend(
            p.ids
                .into_iter()
                .map(|r| (r.namespace, r.id, r.revision, r.schema)),
        );
        spec.cursor = p.next;
        if spec.cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    spec.cursor = None;
    spec.filter = Predicate::Eq(
        FieldRef {
            schema: sr(1),
            position: 0,
        },
        Value::UInt(999),
    );
    assert!(q.query_ids(&subject(), &spec).unwrap().ids.is_empty());
    spec.filter = Predicate::Eq(
        FieldRef {
            schema: sr(2),
            position: 0,
        },
        Value::UInt(2),
    );
    assert_eq!(q.query_ids(&subject(), &spec).unwrap().ids.len(), 1);
}
#[test]
fn selected_values_preserve_schema_order_null_and_non_scalar_types_after_registry_drop() {
    let (_directory, r, mut q) = setup();
    let selection = BTreeMap::from([
        (sr(1), vec![2, 0, 3, 4, 5, 6]),
        (sr(2), vec![7, 2, 0, 3, 4, 5, 6]),
    ]);
    let full = q.query(&subject(), &request()).unwrap().records;
    let expected: Vec<_> = full
        .iter()
        .map(|record| {
            let v = r.decode_values(record.schema, &record.payload).unwrap();
            selection[&record.schema]
                .iter()
                .map(|i| v[*i].clone())
                .collect::<Vec<_>>()
        })
        .collect();
    drop(r);
    let actual = q
        .query_projected(&subject(), &request(), &selection)
        .unwrap();
    assert_eq!(actual.rows.len(), 3);
    for (row, expected) in actual.rows.iter().zip(expected) {
        assert_eq!(row.fields, expected);
    }
    assert_eq!(actual.rows[0].fields[0], None);
    assert_eq!(actual.rows[1].fields[0], None);
}
#[test]
fn cursor_shape_columns_subject_and_snapshot_bindings_are_not_interchangeable() {
    let (_directory, _, mut q) = setup();
    let mut spec = request();
    spec.limit = 1;
    let ids = q.query_ids(&subject(), &spec).unwrap();
    spec.cursor = ids.next;
    assert!(matches!(
        q.query(&subject(), &spec),
        Err(Error::InvalidCursor)
    ));
    let selection = BTreeMap::from([(sr(1), vec![0, 2]), (sr(2), vec![0, 2])]);
    assert!(matches!(
        q.query_projected(&subject(), &spec, &selection),
        Err(Error::InvalidCursor)
    ));
    spec.cursor = None;
    spec.cursor = q
        .query_projected(&subject(), &spec, &selection)
        .unwrap()
        .next;
    let changed = BTreeMap::from([(sr(1), vec![2, 0]), (sr(2), vec![0, 2])]);
    assert!(matches!(
        q.query_projected(&subject(), &spec, &changed),
        Err(Error::InvalidCursor)
    ));
    assert!(matches!(
        q.query_ids(&subject(), &spec),
        Err(Error::InvalidCursor)
    ));
    let mut broad = subject();
    broad.readable_namespaces.insert(2);
    assert!(matches!(
        q.query_projected(&broad, &spec, &selection),
        Err(Error::InvalidCursor)
    ));
}
#[test]
fn authorization_precedes_projection_validation_and_coverage_is_not_silently_omitted() {
    let (_directory, _, mut q) = setup();
    let mut denied = subject();
    denied.readable_namespaces.clear();
    let invalid = BTreeMap::from([(sr(999), vec![999, 999])]);
    assert!(matches!(
        q.query_projected(&denied, &request(), &invalid),
        Err(Error::Unauthorized)
    ));
    assert!(
        q.query_projected(&subject(), &request(), &BTreeMap::from([(sr(1), vec![0])]))
            .is_err()
    );
    for selection in [
        BTreeMap::new(),
        BTreeMap::from([(sr(1), vec![]), (sr(2), vec![0])]),
        BTreeMap::from([(sr(1), vec![0, 0]), (sr(2), vec![0])]),
        BTreeMap::from([(sr(1), vec![999]), (sr(2), vec![0])]),
    ] {
        assert!(
            q.query_projected(&subject(), &request(), &selection)
                .is_err()
        );
    }
    let too_many = BTreeMap::from([(sr(1), vec![0; 257]), (sr(2), vec![0])]);
    assert!(matches!(
        q.query_projected(&subject(), &request(), &too_many),
        Err(Error::TooLarge)
    ));
}

#[test]
fn query_retains_original_snapshot_lease_and_decoder_after_update_compact_and_drop() {
    let (_directory, mut r, mut q) = setup();
    let prior = r.record(1, 1).unwrap().clone();
    let mut values = r.decode_values(prior.schema, &prior.payload).unwrap();
    values[0] = Some(Value::UInt(77));
    r.update(
        NewRecord {
            namespace: 1,
            id: 1,
            schema: sr(1),
            values,
        },
        1,
    )
    .unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert!(r.revision(1, 1, 1).is_some());
    drop(r);
    let ids = q.query_ids(&subject(), &request()).unwrap();
    assert_eq!(ids.ids[0].revision, 1);
    let selection = BTreeMap::from([(sr(1), vec![0]), (sr(2), vec![0])]);
    let page = q
        .query_projected(&subject(), &request(), &selection)
        .unwrap();
    assert_eq!(page.rows[0].fields, vec![Some(Value::UInt(1))]);
    assert_eq!(q.query(&subject(), &request()).unwrap().records[0], prior);
}
#[test]
fn projection_coverage_uses_every_visible_schema_before_filter_and_page() {
    let (_directory, _, mut q) = setup();
    let mut spec = request();
    spec.schemas.clear();
    spec.limit = 1;
    spec.filter = Predicate::Eq(
        FieldRef {
            schema: sr(1),
            position: 0,
        },
        Value::UInt(1),
    );
    assert!(
        q.query_projected(&subject(), &spec, &BTreeMap::from([(sr(1), vec![0])]))
            .is_err()
    );
    let selection = BTreeMap::from([(sr(1), vec![0]), (sr(2), vec![0])]);
    assert_eq!(
        q.query_projected(&subject(), &spec, &selection)
            .unwrap()
            .rows
            .len(),
        1
    );
    let too_many = (1..=257)
        .map(|id| {
            (
                SchemaRef {
                    namespace: 1,
                    id,
                    version: 1,
                },
                vec![0],
            )
        })
        .collect();
    assert!(matches!(
        q.query_projected(&subject(), &spec, &too_many),
        Err(Error::TooLarge)
    ));
}
#[test]
fn native_hidden_endpoints_are_removed_before_coverage_ids_projection_and_counts() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("native{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    let node = sr(1);
    let edge = SchemaRef {
        namespace: 1,
        id: 3,
        version: 1,
    };
    r.register_schema(Schema {
        reference: node,
        fields: vec![Field {
            label: "number".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    r.register_schema(Schema {
        reference: edge,
        fields: vec![Field {
            label: "edge".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.register_relation_rule(RelationRule {
        schema: edge,
        kind: 1,
        source_schema: node,
        target_schema: node,
    })
    .unwrap();
    r.create(1, 1, node, &[Some(Value::UInt(1))]).unwrap();
    r.create(2, 2, node, &[Some(Value::UInt(2))]).unwrap();
    r.create(
        1,
        100,
        edge,
        &[Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 1,
            target: Reference {
                namespace: 2,
                id: 2,
            },
        })],
    )
    .unwrap();
    let mut q = QueryEngine::new(&r, r.snapshot()).unwrap();
    let mut spec = request();
    spec.schemas.clear();
    let selection = BTreeMap::from([(node, vec![0])]);
    let page = q.query_projected(&subject(), &spec, &selection).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.plan.candidates, 1);
    assert!(page.next.is_none());
    assert_eq!(q.query_ids(&subject(), &spec).unwrap().ids.len(), 1);
    let mut broad = subject();
    broad.readable_namespaces.insert(2);
    assert!(q.query_projected(&broad, &spec, &selection).is_err());
    let mut selection = selection;
    selection.insert(edge, vec![0]);
    let p = q.query_projected(&broad, &spec, &selection).unwrap();
    assert_eq!(p.rows.len(), 2);
    assert!(matches!(&p.rows[1].fields[0],Some(Value::Relation{target,..})if target.namespace==2));
}

#[test]
fn selected_pages_keep_descending_identity_ties_and_empty_results() {
    let (_directory, _, mut q) = setup();
    let selection = BTreeMap::from([(sr(1), vec![0]), (sr(2), vec![0])]);
    let mut spec = request();
    for (order, expected) in [
        (
            Order {
                field: None,
                descending: true,
            },
            vec![3, 2, 1],
        ),
        (
            Order {
                field: Some(FieldRef {
                    schema: sr(1),
                    position: 2,
                }),
                descending: false,
            },
            vec![1, 2, 3],
        ),
        (
            Order {
                field: Some(FieldRef {
                    schema: sr(1),
                    position: 2,
                }),
                descending: true,
            },
            vec![3, 1, 2],
        ),
    ] {
        spec.order = order;
        spec.limit = 1;
        spec.cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = q.query_ids(&subject(), &spec).unwrap();
            ids.extend(page.ids.iter().map(|r| r.id));
            spec.cursor = page.next;
            if spec.cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids, expected);
        spec.cursor = None;
        let mut ids = Vec::new();
        loop {
            let page = q.query_projected(&subject(), &spec, &selection).unwrap();
            ids.extend(page.rows.iter().map(|r| r.identity.id));
            spec.cursor = page.next;
            if spec.cursor.is_none() {
                break;
            }
        }
        assert_eq!(ids, expected);
    }
    spec.cursor = None;
    spec.filter = Predicate::Eq(
        FieldRef {
            schema: sr(1),
            position: 0,
        },
        Value::UInt(999),
    );
    assert!(
        q.query_projected(&subject(), &spec, &selection)
            .unwrap()
            .rows
            .is_empty()
    );
}
#[test]
fn new_mode_cursors_reject_other_instances_and_tampered_tokens() {
    let (_directory, r, mut q) = setup();
    let mut spec = request();
    spec.limit = 1;
    let token = q.query_ids(&subject(), &spec).unwrap().next.unwrap();
    spec.cursor = Some(token.clone());
    let mut other = QueryEngine::new(&r, r.snapshot()).unwrap();
    assert!(matches!(
        other.query_ids(&subject(), &spec),
        Err(Error::InvalidCursor)
    ));
    let mut bytes = token.to_bytes();
    bytes[0] ^= 1;
    spec.cursor = Some(Cursor::from_bytes(bytes));
    assert!(matches!(
        q.query_ids(&subject(), &spec),
        Err(Error::InvalidCursor)
    ));
    let selection = BTreeMap::from([(sr(1), vec![0]), (sr(2), vec![0])]);
    spec.cursor = None;
    let token = q
        .query_projected(&subject(), &spec, &selection)
        .unwrap()
        .next
        .unwrap();
    spec.cursor = Some(token);
    assert!(matches!(
        other.query_projected(&subject(), &spec, &selection),
        Err(Error::InvalidCursor)
    ));
}
