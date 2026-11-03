use canonical_core::Error;
use canonical_registry::*;
use canonical_views::*;
use std::{
    cell::Cell,
    io::{self, Write},
};

fn sr(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "views".into(),
        level: Level::Local,
    })
    .unwrap();
    r.register_schema(Schema {
        reference: sr(1),
        fields: vec![
            Field {
                label: "text".into(),
                kind: FieldType::Utf8,
                optional: false,
            },
            Field {
                label: "count".into(),
                kind: FieldType::UInt,
                optional: false,
            },
            Field {
                label: "optional".into(),
                kind: FieldType::Utf8,
                optional: true,
            },
        ],
    })
    .unwrap();
    for id in 1..=3 {
        r.create(
            1,
            id,
            sr(1),
            &[
                Some(Value::Utf8("ação, \"oi\"\n日本語".into())),
                Some(Value::UInt(if id == 1 { u64::MAX } else { id })),
                None,
            ],
        )
        .unwrap();
    }
    r
}

#[test]
fn json_cardinality_is_independent_of_number_of_matches() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut out = Vec::new();
    let stats = reader.write_one(Format::Json, None, &mut out).unwrap();
    assert_eq!(out, b"null");
    assert_eq!(stats.records, 0);
    out.clear();
    reader
        .write_one(Format::Json, r.record(1, 1), &mut out)
        .unwrap();
    let object: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(object.is_object());
    assert_eq!(object["fields"]["text"], "ação, \"oi\"\n日本語");
    assert_eq!(object["fields"]["count"].as_u64(), Some(u64::MAX));
    assert!(object["fields"]["optional"].is_null());
    for count in [0, 1, 3] {
        out.clear();
        let records = (1..=count).map(|id| Ok(r.record(1, id).unwrap().clone()));
        let stats = reader.write_many(Format::Json, records, &mut out).unwrap();
        let array: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(array.as_array().unwrap().len(), count as usize);
        assert_eq!(stats.records, count);
        assert_eq!(stats.bytes, out.len() as u64);
    }
}

#[test]
fn csv_has_headers_for_empty_single_and_multiple_rows_with_exact_escaping() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let format = Format::Csv {
        schema: sr(1),
        nested: CsvNestedPolicy::Reject,
    };
    let mut empty = Vec::new();
    reader
        .write_many(
            format,
            std::iter::empty::<canonical_core::Result<Record>>(),
            &mut empty,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(empty.clone()).unwrap(),
        "\"namespace\",\"id\",\"schema_namespace\",\"schema_id\",\"schema_version\",\"revision\",\"field.text\",\"field.count\",\"field.optional\"\r\n"
    );
    let mut single = Vec::new();
    reader
        .write_one(format, r.record(1, 1), &mut single)
        .unwrap();
    let text = String::from_utf8(single.clone()).unwrap();
    assert!(text.contains("\"ação, \"\"oi\"\"\n日本語\""));
    assert!(single.starts_with(&empty));
    let mut many = Vec::new();
    let stats = reader
        .write_many(
            format,
            (1..=3).map(|id| Ok(r.record(1, id).unwrap().clone())),
            &mut many,
        )
        .unwrap();
    assert_eq!(stats.records, 3);
    assert!(many.starts_with(&single));
}

struct FailAfter {
    remaining: usize,
}
impl Write for FailAfter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "sentinel sink"));
        }
        let count = self.remaining.min(bytes.len());
        self.remaining -= count;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        panic!("reader must not flush the caller's sink")
    }
}

#[test]
fn output_failure_and_limits_stop_before_consuming_the_collection() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let consumed = Cell::new(0usize);
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let source = (0..1_000_000).map(|_| {
        consumed.set(consumed.get() + 1);
        Ok(r.record(1, 1).unwrap().clone())
    });
    let result = reader.write_many(Format::Json, source, &mut FailAfter { remaining: 50 });
    assert!(matches!(result,Err(Error::Io(ref error)) if error.kind()==io::ErrorKind::BrokenPipe));
    assert!(consumed.get() <= 1);
    let reader = Reader::new(
        &r,
        Limits {
            max_records: 10,
            max_output_bytes: 2,
        },
    )
    .unwrap();
    let mut output = Vec::new();
    assert!(matches!(
        reader.write_one(Format::Json, r.record(1, 1), &mut output),
        Err(Error::TooLarge)
    ));
    assert!(output.len() <= 2);
    let reader = Reader::new(
        &r,
        Limits {
            max_records: 1,
            max_output_bytes: 1024 * 1024,
        },
    )
    .unwrap();
    assert!(matches!(
        reader.write_many(
            Format::Json,
            (1..=2).map(|id| Ok(r.record(1, id).unwrap().clone())),
            &mut Vec::new()
        ),
        Err(Error::TooLarge)
    ));
    assert!(
        Reader::new(
            &r,
            Limits {
                max_records: 0,
                max_output_bytes: 1
            }
        )
        .is_err()
    );
}

#[test]
fn cbr_roundtrip_is_incremental_and_rejects_truncation_corruption_and_bad_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let records: Vec<_> = (1..=3).map(|id| r.record(1, id).unwrap().clone()).collect();
    let mut bytes = Vec::new();
    reader
        .write_many(Format::Cbr, records.iter().cloned().map(Ok), &mut bytes)
        .unwrap();
    assert_eq!(
        CbrReader::new(bytes.as_slice())
            .collect::<canonical_core::Result<Vec<_>>>()
            .unwrap(),
        records
    );
    let mut one = Vec::new();
    reader
        .write_one(Format::Cbr, Some(&records[0]), &mut one)
        .unwrap();
    assert!(CbrReader::new(&b""[..]).next().is_none());
    for cut in 1..one.len() {
        let mut decoder = CbrReader::new(&one[..cut]);
        assert!(decoder.next().unwrap().is_err(), "cut={cut}");
        assert!(decoder.next().is_none());
    }
    let mut corrupt = one.clone();
    let end = corrupt.len();
    corrupt[end - 1] ^= 1;
    let mut decoder = CbrReader::new(corrupt.as_slice());
    assert!(decoder.next().unwrap().is_err());
    assert!(decoder.next().is_none());
    let (_, mut payload, _) = canonical_codec::decode_frame(&one, 0).unwrap();
    payload[0] ^= 1;
    let bad = canonical_codec::encode_frame(1, &payload).unwrap();
    assert!(CbrReader::new(bad.as_slice()).next().unwrap().is_err());
}

#[test]
fn requested_readers_do_not_create_or_modify_database_views() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let manifest = std::fs::read(dir.path().join("manifest.cbr")).unwrap();
    let segment = std::fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    let before: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut output = Vec::new();
    reader
        .write_one(Format::Json, r.record(1, 1), &mut output)
        .unwrap();
    let after: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
    assert_eq!(
        manifest,
        std::fs::read(dir.path().join("manifest.cbr")).unwrap()
    );
    assert_eq!(
        segment,
        std::fs::read(dir.path().join("segments/00000001.cbr")).unwrap()
    );
}

#[test]
fn nested_values_have_explicit_json_and_csv_policy_and_cbr_stays_lossless() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.register_schema(Schema {
        reference: sr(2),
        fields: vec![Field {
            label: "child".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    let kinds = vec![
        FieldType::Struct(sr(2)),
        FieldType::Array {
            element: ScalarType::UInt,
            max_items: 10,
        },
        FieldType::Ref,
        FieldType::Relation,
        FieldType::Bytes,
        FieldType::Decimal { scale: 18 },
        FieldType::FixedInt,
        FieldType::Float,
        FieldType::Bool,
        FieldType::Enum {
            variants: vec![1, 2],
        },
    ];
    r.register_schema(Schema {
        reference: sr(3),
        fields: kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Field {
                label: format!("f{i}"),
                kind,
                optional: false,
            })
            .collect(),
    })
    .unwrap();
    let inner = r
        .encode_values(sr(2), &[Some(Value::Utf8("interno\nالعربية".into()))])
        .unwrap();
    let values = vec![
        Value::Struct(inner),
        Value::Array(vec![ScalarValue::UInt(1), ScalarValue::UInt(u64::MAX)]),
        Value::Ref(Reference {
            namespace: 1,
            id: 2,
        }),
        Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 7,
            target: Reference {
                namespace: 1,
                id: 2,
            },
        },
        Value::Bytes(vec![0, 255]),
        Value::Decimal {
            mantissa: i64::MIN,
            scale: 18,
        },
        Value::FixedInt(i64::MIN),
        Value::Float(0.5),
        Value::Bool(true),
        Value::Enum(2),
    ];
    let record = r
        .create(
            1,
            10,
            sr(3),
            &values.into_iter().map(Some).collect::<Vec<_>>(),
        )
        .unwrap();
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut output = Vec::new();
    reader
        .write_one(Format::Json, Some(&record), &mut output)
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["fields"]["f0"]["child"], "interno\nالعربية");
    assert_eq!(json["fields"]["f1"][1].as_u64(), Some(u64::MAX));
    assert_eq!(json["fields"]["f2"]["id"], 2);
    assert_eq!(json["fields"]["f3"]["kind"], 7);
    assert_eq!(json["fields"]["f4"][1], 255);
    assert_eq!(json["fields"]["f5"]["mantissa"].as_i64(), Some(i64::MIN));
    assert!(matches!(
        reader.write_one(
            Format::Csv {
                schema: sr(3),
                nested: CsvNestedPolicy::Reject
            },
            Some(&record),
            &mut Vec::new()
        ),
        Err(Error::InvalidInput(_))
    ));
    output.clear();
    reader
        .write_one(
            Format::Csv {
                schema: sr(3),
                nested: CsvNestedPolicy::JsonCell,
            },
            Some(&record),
            &mut output,
        )
        .unwrap();
    let csv = String::from_utf8(output).unwrap();
    assert!(csv.contains("\"\"child\"\""));
    assert!(csv.contains("-9.223372036854775808"));
    let mut binary = Vec::new();
    reader
        .write_one(Format::Cbr, Some(&record), &mut binary)
        .unwrap();
    let decoded = CbrReader::new(binary.as_slice()).next().unwrap().unwrap();
    assert_eq!(decoded, record);
    assert_eq!(
        r.decode_values(decoded.schema, &decoded.payload).unwrap(),
        r.decode_values(record.schema, &record.payload).unwrap()
    );
}

#[test]
fn input_errors_and_csv_schema_mismatch_do_not_silently_drop_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.register_schema(Schema {
        reference: sr(2),
        fields: vec![],
    })
    .unwrap();
    let other = r.create(1, 10, sr(2), &[]).unwrap();
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut output = Vec::new();
    assert!(
        reader
            .write_many(
                Format::Csv {
                    schema: sr(1),
                    nested: CsvNestedPolicy::Reject
                },
                [Ok(r.record(1, 1).unwrap().clone()), Ok(other)],
                &mut output
            )
            .is_err()
    );
    assert!(matches!(
        reader.write_many(Format::Json, [Err(Error::Unauthorized)], &mut Vec::new()),
        Err(Error::Unauthorized)
    ));
    let mut malformed = r.record(1, 1).unwrap().clone();
    malformed.payload = vec![];
    assert!(
        reader
            .write_one(Format::Json, Some(&malformed), &mut Vec::new())
            .is_err()
    );
}

#[test]
fn frozen_cbr_view_v1_remains_decodable_with_its_schema_contract() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "views.example".into(),
        level: Level::Local,
    })
    .unwrap();
    r.register_schema(Schema {
        reference: sr(1),
        fields: vec![
            Field {
                label: "text".into(),
                kind: FieldType::Utf8,
                optional: false,
            },
            Field {
                label: "number".into(),
                kind: FieldType::UInt,
                optional: false,
            },
        ],
    })
    .unwrap();
    let rows = CbrReader::new(&include_bytes!("fixtures/view-v1.cbr")[..])
        .collect::<canonical_core::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        r.decode_values(rows[0].schema, &rows[0].payload).unwrap(),
        vec![
            Some(Value::Utf8("ação, \"oi\"\n日本語".into())),
            Some(Value::UInt(u64::MAX))
        ]
    );
    assert_eq!(
        r.decode_values(rows[1].schema, &rows[1].payload).unwrap(),
        vec![Some(Value::Utf8("العربية".into())), Some(Value::UInt(871))]
    );
}
