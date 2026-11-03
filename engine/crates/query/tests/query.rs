use canonical_core::Error;
use canonical_query::*;
use canonical_registry::*;
use std::collections::BTreeSet;

fn schema() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn field(position: usize) -> FieldRef {
    FieldRef {
        schema: schema(),
        position,
    }
}
fn subject() -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    }
}
fn spec(filter: Predicate) -> Query {
    Query {
        namespaces: vec![1],
        schemas: vec![schema()],
        filter,
        order: Order::default(),
        limit: 1000,
        cursor: None,
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
    r.register_schema(Schema {
        reference: schema(),
        fields: [
            ("action", FieldType::UInt),
            ("file", FieldType::UInt),
            ("objective", FieldType::UInt),
            ("task", FieldType::UInt),
            ("worker", FieldType::UInt),
            (
                "state",
                FieldType::Enum {
                    variants: vec![1, 2],
                },
            ),
            ("timestamp_ms_utc", FieldType::UInt),
        ]
        .into_iter()
        .map(|(label, kind)| Field {
            label: label.into(),
            kind,
            optional: false,
        })
        .collect(),
    })
    .unwrap();
    let records: Vec<_> = (1..=100)
        .map(|id| NewRecord {
            namespace: if id <= 90 { 1 } else { 2 },
            id,
            schema: schema(),
            values: vec![
                Some(Value::UInt(id % 3)),
                Some(Value::UInt(id % 10)),
                Some(Value::UInt(id % 4)),
                Some(Value::UInt(id % 5)),
                Some(Value::UInt(id % 2)),
                Some(Value::Enum(if id % 2 == 0 { 2 } else { 1 })),
                Some(Value::UInt(1000 + id)),
            ],
        })
        .collect();
    r.create_batch([1; 16], &records).unwrap();
    r
}

#[test]
fn all_context_filters_match_independent_scan_oracle() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let snap = r.snapshot();
    let mut engine = QueryEngine::new(&r, snap.clone()).unwrap();
    for (position, desired) in [
        (0, Value::UInt(1)),
        (1, Value::UInt(2)),
        (2, Value::UInt(3)),
        (3, Value::UInt(4)),
        (4, Value::UInt(1)),
        (5, Value::Enum(2)),
        (6, Value::UInt(1050)),
    ] {
        let expected: Vec<_> = snap
            .iter()
            .filter(|record| record.namespace == 1)
            .filter(|record| {
                r.decode_values(record.schema, &record.payload).unwrap()[position]
                    == Some(desired.clone())
            })
            .cloned()
            .collect();
        let page = engine
            .query(&subject(), &spec(Predicate::Eq(field(position), desired)))
            .unwrap();
        assert_eq!(page.records, expected);
        assert_eq!(page.plan.strategy, PlanStrategy::Indexed);
        assert_eq!(page.plan.rows_examined, expected.len());
    }
}

#[test]
fn boolean_filters_ranges_and_pages_preserve_stable_order() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let predicate = Predicate::And(vec![
        Predicate::Range {
            field: field(6),
            lower: Some(Value::UInt(1020)),
            upper: Some(Value::UInt(1060)),
        },
        Predicate::Or(vec![
            Predicate::Eq(field(1), Value::UInt(2)),
            Predicate::Eq(field(1), Value::UInt(3)),
        ]),
    ]);
    let expected: Vec<_> = (20..=60)
        .filter(|id| id % 10 == 2 || id % 10 == 3)
        .collect();
    let mut request = spec(predicate);
    request.limit = 1;
    let mut ids = Vec::new();
    loop {
        let page = engine.query(&subject(), &request).unwrap();
        ids.extend(page.records.iter().map(|r| r.id));
        request.cursor = page.next;
        if request.cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, expected);
    let mut empty = spec(Predicate::Eq(field(0), Value::UInt(999)));
    assert!(engine.query(&subject(), &empty).unwrap().records.is_empty());
    empty.filter = Predicate::Eq(field(6), Value::UInt(1050));
    assert_eq!(engine.query(&subject(), &empty).unwrap().records.len(), 1);
}

#[test]
fn authorization_is_deny_by_default_and_precedes_data_access() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let mut e = QueryEngine::new(&r, r.snapshot()).unwrap();
    let mut request = spec(Predicate::True);
    assert!(matches!(
        e.query(
            &Subject {
                id: "reader".into(),
                readable_namespaces: BTreeSet::new()
            },
            &request
        ),
        Err(Error::Unauthorized)
    ));
    request.namespaces = vec![2];
    assert!(matches!(
        e.query(&subject(), &request),
        Err(Error::Unauthorized)
    ));
    request.namespaces.clear();
    assert!(matches!(
        e.query(&subject(), &request),
        Err(Error::Unauthorized)
    ));
    request.namespaces = vec![1];
    let page = e.query(&subject(), &request).unwrap();
    assert_eq!(page.records.len(), 90);
    assert!(page.records.iter().all(|r| r.namespace == 1));
}

#[test]
fn mismatched_types_and_invalid_cursors_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let snapshot = r.snapshot();
    let mut e = QueryEngine::new(&r, snapshot.clone()).unwrap();
    assert!(matches!(
        e.query(
            &subject(),
            &spec(Predicate::Eq(field(0), Value::Utf8("1".into())))
        ),
        Err(Error::InvalidInput(_))
    ));
    assert!(
        e.query(
            &subject(),
            &spec(Predicate::Range {
                field: field(6),
                lower: Some(Value::UInt(20)),
                upper: Some(Value::UInt(10))
            })
        )
        .is_err()
    );
    let mut request = spec(Predicate::True);
    request.limit = 2;
    let cursor = e.query(&subject(), &request).unwrap().next.unwrap();
    let mut altered = cursor.to_bytes();
    altered[0] ^= 128;
    request.cursor = Some(Cursor::from_bytes(altered));
    assert!(matches!(
        e.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    request.cursor = Some(cursor.clone());
    request.filter = Predicate::Eq(field(0), Value::UInt(1));
    assert!(matches!(
        e.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    request.filter = Predicate::True;
    let mut other = QueryEngine::new(&r, snapshot).unwrap();
    assert!(matches!(
        other.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
    r.delete(1, 1, 1).unwrap(); // engine remains on the earlier consistent snapshot
    let page = e.query(&subject(), &request).unwrap();
    assert_eq!(
        page.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![3, 4]
    );
    let mut updated = QueryEngine::new(&r, r.snapshot()).unwrap();
    assert!(matches!(
        updated.query(&subject(), &request),
        Err(Error::InvalidCursor)
    ));
}

#[test]
fn field_order_has_stable_identity_ties() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let mut request = spec(Predicate::True);
    request.order = Order {
        field: Some(field(1)),
        descending: true,
    };
    request.limit = 7;
    let mut expected: Vec<_> = (1..=90).collect();
    expected.sort_by(|a, b| (b % 10).cmp(&(a % 10)).then(a.cmp(b)));
    let mut actual = Vec::new();
    loop {
        let page = engine.query(&subject(), &request).unwrap();
        actual.extend(page.records.iter().map(|r| r.id));
        request.cursor = page.next;
        if request.cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn scalar_domains_optional_order_and_unsupported_fields_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let sr = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    let kinds = vec![
        FieldType::UInt,
        FieldType::FixedInt,
        FieldType::Decimal { scale: 2 },
        FieldType::Float,
        FieldType::Utf8,
        FieldType::Bool,
        FieldType::Bytes,
        FieldType::Ref,
        FieldType::Array {
            element: ScalarType::UInt,
            max_items: 2,
        },
    ];
    r.register_schema(Schema {
        reference: sr,
        fields: kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Field {
                label: format!("f{i}"),
                kind,
                optional: i == 0,
            })
            .collect(),
    })
    .unwrap();
    let values = vec![
        vec![
            None,
            Some(Value::FixedInt(-5)),
            Some(Value::Decimal {
                mantissa: -120,
                scale: 2,
            }),
            Some(Value::Float(-0.0)),
            Some(Value::Utf8("a".into())),
            Some(Value::Bool(false)),
            Some(Value::Bytes(vec![0])),
            Some(Value::Ref(Reference {
                namespace: 1,
                id: 1,
            })),
            Some(Value::Array(vec![ScalarValue::UInt(1)])),
        ],
        vec![
            Some(Value::UInt(2)),
            Some(Value::FixedInt(0)),
            Some(Value::Decimal {
                mantissa: -110,
                scale: 2,
            }),
            Some(Value::Float(0.0)),
            Some(Value::Utf8("é".into())),
            Some(Value::Bool(true)),
            Some(Value::Bytes(vec![255])),
            Some(Value::Ref(Reference {
                namespace: 1,
                id: 2,
            })),
            Some(Value::Array(vec![ScalarValue::UInt(2)])),
        ],
        vec![
            Some(Value::UInt(1)),
            Some(Value::FixedInt(7)),
            Some(Value::Decimal {
                mantissa: 150,
                scale: 2,
            }),
            Some(Value::Float(1.5)),
            Some(Value::Utf8("日本語".into())),
            Some(Value::Bool(true)),
            Some(Value::Bytes(vec![1])),
            Some(Value::Ref(Reference {
                namespace: 1,
                id: 3,
            })),
            Some(Value::Array(vec![ScalarValue::UInt(3)])),
        ],
    ];
    let records: Vec<_> = values
        .into_iter()
        .enumerate()
        .map(|(i, values)| NewRecord {
            namespace: 1,
            id: 201 + i as u64,
            schema: sr,
            values,
        })
        .collect();
    r.create_batch([2; 16], &records).unwrap();
    let mut e = QueryEngine::new(&r, r.snapshot()).unwrap();
    let f = |position| FieldRef {
        schema: sr,
        position,
    };
    let mut q = spec(Predicate::True);
    q.schemas = vec![sr];
    for (position, value, expected) in [
        (1, Value::FixedInt(-5), vec![201]),
        (3, Value::Float(0.0), vec![201, 202]),
        (5, Value::Bool(true), vec![202, 203]),
        (6, Value::Bytes(vec![255]), vec![202]),
        (
            7,
            Value::Ref(Reference {
                namespace: 1,
                id: 3,
            }),
            vec![203],
        ),
    ] {
        q.filter = Predicate::Eq(f(position), value);
        assert_eq!(
            e.query(&subject(), &q)
                .unwrap()
                .records
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            expected
        );
    }
    q.filter = Predicate::Range {
        field: f(2),
        lower: Some(Value::Decimal {
            mantissa: -115,
            scale: 2,
        }),
        upper: Some(Value::Decimal {
            mantissa: 200,
            scale: 2,
        }),
    };
    assert_eq!(
        e.query(&subject(), &q)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![202, 203]
    );
    q.filter = Predicate::True;
    q.order = Order {
        field: Some(f(0)),
        descending: false,
    };
    assert_eq!(
        e.query(&subject(), &q)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![201, 203, 202]
    );
    q.order.descending = true;
    assert_eq!(
        e.query(&subject(), &q)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec![202, 203, 201]
    );
    q.order = Order::default();
    for bad in [
        Predicate::Eq(f(8), Value::Array(vec![])),
        Predicate::Eq(
            f(2),
            Value::Decimal {
                mantissa: 1,
                scale: 1,
            },
        ),
        Predicate::Eq(f(3), Value::Float(f64::NAN)),
        Predicate::Eq(
            f(7),
            Value::Ref(Reference {
                namespace: 1,
                id: 0,
            }),
        ),
    ] {
        q.filter = bad;
        assert!(matches!(
            e.query(&subject(), &q),
            Err(Error::InvalidInput(_))
        ));
    }
}

#[test]
fn cursor_binding_rechecks_subject_scope_and_page_shape() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let mut e = QueryEngine::new(&r, r.snapshot()).unwrap();
    let mut q = spec(Predicate::True);
    q.limit = 1;
    let cursor = e.query(&subject(), &q).unwrap().next.unwrap();
    q.cursor = Some(cursor);
    let other = Subject {
        id: "other".into(),
        ..subject()
    };
    assert!(matches!(e.query(&other, &q), Err(Error::InvalidCursor)));
    let expanded = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1, 2]),
    };
    assert!(matches!(e.query(&expanded, &q), Err(Error::InvalidCursor)));
    let revoked = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::new(),
    };
    assert!(matches!(e.query(&revoked, &q), Err(Error::Unauthorized)));
    q.limit = 2;
    assert!(matches!(e.query(&subject(), &q), Err(Error::InvalidCursor)));
    q.cursor = None;
    q.limit = 0;
    assert!(matches!(
        e.query(&subject(), &q),
        Err(Error::InvalidInput(_))
    ));
    q.limit = 1001;
    assert!(matches!(
        e.query(&subject(), &q),
        Err(Error::InvalidInput(_))
    ));
    q.limit = 100;
    q.order = Order {
        field: None,
        descending: true,
    };
    assert_eq!(
        e.query(&subject(), &q).unwrap().records.first().unwrap().id,
        90
    );
}
