use canonical_core::Error;
use canonical_registry::*;

fn schema(version: u32) -> Schema {
    Schema {
        reference: SchemaRef {
            namespace: 7,
            id: 12,
            version,
        },
        fields: vec![Field {
            label: "ação 日本語 العربية".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    r.register_namespace(Namespace {
        id: 7,
        name: "plugin.example".into(),
        level: Level::Plugin,
    })
    .unwrap();
    r.register_schema(schema(1)).unwrap();
    r
}

#[test]
fn old_schema_and_records_survive_evolution_and_plugin_absence() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let old = schema(1).reference;
    r.create(
        7,
        55,
        old,
        &[Some(Value::Utf8("criar 日本語 العربية".into()))],
    )
    .unwrap();
    let mut new = schema(2);
    new.fields.push(Field {
        label: "file".into(),
        kind: FieldType::UInt,
        optional: true,
    });
    let new_ref = new.reference;
    r.register_schema(new).unwrap();
    r.create(
        7,
        56,
        new_ref,
        &[Some(Value::Utf8("modificar".into())), None],
    )
    .unwrap();
    drop(r);
    // No plugin object or executable is supplied on reopen.
    let r = Registry::open(dir.path()).unwrap();
    assert_eq!(
        r.schema(old).unwrap().fields[0].label,
        "ação 日本語 العربية"
    );
    assert_eq!(
        r.decode_record(7, 55).unwrap()[0],
        Some(Value::Utf8("criar 日本語 العربية".into()))
    );
    assert_eq!(r.decode_record(7, 56).unwrap()[1], None);
}

#[test]
fn stable_ids_cannot_be_redefined_and_invalid_payload_is_not_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let mut changed = schema(1);
    changed.fields[0].kind = FieldType::Bool;
    assert!(matches!(r.register_schema(changed), Err(Error::Conflict)));
    let reference = schema(1).reference;
    assert!(r.create(7, 1, reference, &[None]).is_err());
    assert!(r.create(7, 1, reference, &[Some(Value::UInt(9))]).is_err());
    assert!(matches!(
        r.create(
            7,
            1,
            SchemaRef {
                version: 99,
                ..reference
            },
            &[]
        ),
        Err(Error::UnknownSchema)
    ));
    assert!(r.record(7, 1).is_none());
    drop(r);
    assert!(Registry::open(dir.path()).unwrap().record(7, 1).is_none());
}

#[test]
fn all_value_types_roundtrip_and_bitmap_is_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let child = r
        .encode_values(schema(1).reference, &[Some(Value::Utf8("nested".into()))])
        .unwrap();
    let kinds = vec![
        FieldType::Bool,
        FieldType::UInt,
        FieldType::Int,
        FieldType::Decimal { scale: 2 },
        FieldType::Float,
        FieldType::Enum {
            variants: vec![2, 3],
        },
        FieldType::Ref,
        FieldType::Utf8,
        FieldType::Bytes,
        FieldType::Array {
            element: ScalarType::UInt,
            max_items: 3,
        },
        FieldType::Struct(schema(1).reference),
        FieldType::Relation,
    ];
    let values = vec![
        Value::Bool(true),
        Value::UInt(871),
        Value::Int(-42),
        Value::Decimal {
            mantissa: -1205,
            scale: 2,
        },
        Value::Float(0.5),
        Value::Enum(2),
        Value::Ref(Reference {
            namespace: 7,
            id: 55,
        }),
        Value::Utf8("unknown 🦀".into()),
        Value::Bytes(vec![0, 255]),
        Value::Array(vec![ScalarValue::UInt(1), ScalarValue::UInt(2)]),
        Value::Struct(child),
        Value::Relation {
            source: Reference {
                namespace: 7,
                id: 1,
            },
            kind: 2,
            target: Reference {
                namespace: 7,
                id: 2,
            },
        },
    ];
    let s = Schema {
        reference: SchemaRef {
            namespace: 7,
            id: 13,
            version: 1,
        },
        fields: kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Field {
                label: i.to_string(),
                kind,
                optional: false,
            })
            .collect(),
    };
    let sr = s.reference;
    r.register_schema(s).unwrap();
    let options: Vec<_> = values.into_iter().map(Some).collect();
    r.create(7, 1, sr, &options).unwrap();
    assert_eq!(r.decode_record(7, 1).unwrap(), options);
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    assert_eq!(r.decode_record(7, 1).unwrap(), options);
}

#[test]
fn namespaces_concepts_and_validation_limits_are_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let c = Concept {
        namespace: 7,
        id: 20,
        version: 1,
        contract: vec![1, 2, 3],
        label: Some("resource.create".into()),
    };
    r.register_concept(c.clone()).unwrap();
    r.register_concept(c.clone()).unwrap();
    assert!(matches!(
        r.register_concept(Concept {
            contract: vec![9],
            ..c
        }),
        Err(Error::Conflict)
    ));
    let mut s = schema(2);
    s.fields[0].kind = FieldType::Array {
        element: ScalarType::UInt,
        max_items: 1,
    };
    let sr = s.reference;
    r.register_schema(s).unwrap();
    assert!(
        r.create(
            7,
            1,
            sr,
            &[Some(Value::Array(vec![
                ScalarValue::UInt(1),
                ScalarValue::UInt(2)
            ]))]
        )
        .is_err()
    );
    assert!(
        r.create(99, 1, schema(1).reference, &[Some(Value::Utf8("x".into()))])
            .is_err()
    );
    assert!(
        r.create(
            7,
            1,
            sr,
            &[Some(Value::Array(vec![ScalarValue::Bool(true)]))]
        )
        .is_err()
    );
}

#[test]
fn fixed_integer_uses_exactly_eight_payload_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let mut s = schema(3);
    s.fields[0].kind = FieldType::FixedInt;
    let sr = s.reference;
    r.register_schema(s).unwrap();
    let encoded = canonical_codec::encode(&Value::FixedInt(-42)).unwrap();
    assert_eq!(encoded.len(), 9); // one enum tag + eight raw little-endian bytes
    r.create(7, 10, sr, &[Some(Value::FixedInt(-42))]).unwrap();
    assert_eq!(
        r.decode_record(7, 10).unwrap(),
        vec![Some(Value::FixedInt(-42))]
    );
}

#[test]
fn frozen_csr_v1_journal_remains_readable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("segments")).unwrap();
    std::fs::write(
        dir.path().join("manifest.cbr"),
        include_bytes!("fixtures/csr-v1/manifest.cbr"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("segments/00000001.cbr"),
        include_bytes!("fixtures/csr-v1/segments/00000001.cbr"),
    )
    .unwrap();
    let registry = Registry::open(dir.path()).unwrap();
    assert_eq!(
        registry.decode_record(1, 1).unwrap(),
        vec![Some(Value::UInt(2))]
    );
}

#[test]
fn malformed_bitmap_and_nonfinite_float_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = setup(dir.path());
    let mut s = schema(9);
    s.fields[0].optional = true;
    let sr = s.reference;
    registry.register_schema(s).unwrap();
    let mut bytes = registry.encode_values(sr, &[None]).unwrap();
    bytes[1] = 128; // reserved optional bit in the persisted bitmap
    assert!(registry.decode_values(sr, &bytes).is_err());
    let mut s = schema(10);
    s.fields[0].kind = FieldType::Float;
    let sr = s.reference;
    registry.register_schema(s).unwrap();
    assert!(
        registry
            .create(7, 1, sr, &[Some(Value::Float(f64::NAN))])
            .is_err()
    );
    assert!(
        registry
            .create(7, 1, sr, &[Some(Value::Float(f64::INFINITY))])
            .is_err()
    );
    assert!(
        registry
            .create(
                7,
                1,
                schema(1).reference,
                &[Some(Value::Utf8("x".repeat(MAX_TEXT_BYTES + 1)))]
            )
            .is_err()
    );
}
