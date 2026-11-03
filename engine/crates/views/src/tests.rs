use super::*;
use canonical_registry::*;
use std::{cell::Cell, io};

fn sr() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn setup() -> (tempfile::TempDir, Registry, Record) {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "views".into(),
        level: Level::Local,
    })
    .unwrap();
    r.register_schema(Schema {
        reference: sr(),
        fields: vec![Field {
            label: "text".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    let record = r
        .create(1, 1, sr(), &[Some(Value::Utf8("a,\"b\"\n日本語".into()))])
        .unwrap();
    (d, r, record)
}
#[test]
fn cardinality_csv_and_cbr_roundtrip() {
    let (_d, r, record) = setup();
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut out = Vec::new();
    reader.write_one(Format::Json, None, &mut out).unwrap();
    assert_eq!(out, b"null");
    out.clear();
    reader
        .write_many(Format::Json, std::iter::empty(), &mut out)
        .unwrap();
    assert_eq!(out, b"[]");
    out.clear();
    reader
        .write_many(Format::Json, [Ok(record.clone())], &mut out)
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(json.is_array());
    assert_eq!(json[0]["fields"]["text"], "a,\"b\"\n日本語");
    out.clear();
    reader
        .write_one(
            Format::Csv {
                schema: sr(),
                nested: CsvNestedPolicy::Reject,
            },
            Some(&record),
            &mut out,
        )
        .unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.contains("\"a,\"\"b\"\"\n日本語\"\r\n"));
    out.clear();
    reader
        .write_many(
            Format::Cbr,
            [Ok(record.clone()), Ok(record.clone())],
            &mut out,
        )
        .unwrap();
    assert_eq!(
        CbrReader::new(out.as_slice())
            .collect::<Result<Vec<_>>>()
            .unwrap(),
        vec![record.clone(), record]
    );
}
#[test]
fn failure_stops_source_and_output_budget_is_enforced() {
    let (_d, r, record) = setup();
    let reader = Reader::new(
        &r,
        Limits {
            max_records: 100,
            max_output_bytes: 8,
        },
    )
    .unwrap();
    let visits = Cell::new(0);
    let source = (0..100).map(|_| {
        visits.set(visits.get() + 1);
        Ok(record.clone())
    });
    let mut out = Vec::new();
    assert!(matches!(
        reader.write_many(Format::Json, source, &mut out),
        Err(Error::TooLarge)
    ));
    assert!(out.len() <= 8);
    assert!(visits.get() <= 1);
    struct Fail;
    impl io::Write for Fail {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "sink"))
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("reader must not flush")
        }
    }
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let mut sink = Fail;
    let result = reader.write_one(Format::Json, Some(&record), &mut sink);
    assert!(matches!(result,Err(Error::Io(e)) if e.kind()==io::ErrorKind::BrokenPipe));
}
#[test]
fn cbr_decoder_is_fused_after_error_and_rejects_every_partial_frame() {
    let (_d, r, record) = setup();
    let mut bytes = Vec::new();
    Reader::new(&r, Limits::default())
        .unwrap()
        .write_one(Format::Cbr, Some(&record), &mut bytes)
        .unwrap();
    for cut in 1..bytes.len() {
        let mut decoder = CbrReader::new(&bytes[..cut]);
        assert!(decoder.next().unwrap().is_err(), "cut {cut}");
        assert!(decoder.next().is_none());
    }
    let mut bad = bytes.clone();
    let n = bad.len();
    bad[n - 1] ^= 1;
    assert!(CbrReader::new(bad.as_slice()).next().unwrap().is_err());
}

#[test]
fn nested_json_and_csv_cells_obey_output_limit_without_collecting_views() {
    let (_d, mut r, _) = setup();
    let child = SchemaRef { id: 2, ..sr() };
    let parent = SchemaRef { id: 3, ..sr() };
    r.register_schema(Schema {
        reference: child,
        fields: (0..64)
            .map(|i| Field {
                label: format!("{i}{}", "x".repeat(1000)),
                kind: FieldType::Bool,
                optional: false,
            })
            .collect(),
    })
    .unwrap();
    r.register_schema(Schema {
        reference: parent,
        fields: vec![Field {
            label: "nested".into(),
            kind: FieldType::Struct(child),
            optional: false,
        }],
    })
    .unwrap();
    let child_bytes = r
        .encode_values(child, &vec![Some(Value::Bool(true)); 64])
        .unwrap();
    let record = r
        .create(1, 2, parent, &[Some(Value::Struct(child_bytes))])
        .unwrap();
    let reader = Reader::new(
        &r,
        Limits {
            max_records: 10,
            max_output_bytes: 256,
        },
    )
    .unwrap();
    for format in [
        Format::Json,
        Format::Csv {
            schema: parent,
            nested: CsvNestedPolicy::JsonCell,
        },
    ] {
        let mut out = Vec::new();
        assert!(matches!(
            reader.write_one(format, Some(&record), &mut out),
            Err(Error::TooLarge)
        ));
        assert!(out.len() <= 256);
    }
    let mut out = Vec::new();
    assert!(matches!(
        reader.write_one(
            Format::Csv {
                schema: parent,
                nested: CsvNestedPolicy::Reject
            },
            Some(&record),
            &mut out
        ),
        Err(Error::InvalidInput(_))
    ));
    let mut out = Vec::new();
    Reader::new(&r, Limits::default())
        .unwrap()
        .write_one(Format::Json, Some(&record), &mut out)
        .unwrap();
    assert!(out.len() > 64_000);
}

#[test]
fn cbr_rejects_sequence_before_body_and_envelope_magic_and_version() {
    use std::io::Read;
    let (_d, r, record) = setup();
    let body = canonical_codec::encode(&(*b"CVW1", 1u16, &record)).unwrap();
    let frame = canonical_codec::encode_frame(2, &body).unwrap();
    struct Count<'a> {
        bytes: &'a [u8],
        read: usize,
    }
    impl Read for Count<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = self.bytes.read(out)?;
            self.read += n;
            Ok(n)
        }
    }
    let mut counted = Count {
        bytes: &frame[..24],
        read: 0,
    };
    {
        let mut decoder = CbrReader::new(&mut counted);
        assert!(matches!(
            decoder.next(),
            Some(Err(Error::Corruption { offset: 0 }))
        ));
        assert!(decoder.next().is_none());
    }
    assert_eq!(counted.read, 24);
    for (magic, version) in [(*b"NOPE", 1u16), (*b"CVW1", 2)] {
        let body = canonical_codec::encode(&(magic, version, &record)).unwrap();
        let frame = canonical_codec::encode_frame(1, &body).unwrap();
        assert!(CbrReader::new(frame.as_slice()).next().unwrap().is_err());
    }
    assert!(r.record(1, 1).is_some());
}

#[test]
fn exact_budget_record_count_and_mixed_csv_schema_are_not_silent() {
    let (_d, mut r, record) = setup();
    let full = Reader::new(&r, Limits::default()).unwrap();
    let mut expected = Vec::new();
    let stats = full
        .write_one(Format::Json, Some(&record), &mut expected)
        .unwrap();
    assert_eq!(stats.bytes, expected.len() as u64);
    let mut out = Vec::new();
    Reader::new(
        &r,
        Limits {
            max_records: 1,
            max_output_bytes: expected.len() as u64,
        },
    )
    .unwrap()
    .write_one(Format::Json, Some(&record), &mut out)
    .unwrap();
    assert_eq!(out, expected);
    let mut out = Vec::new();
    assert!(matches!(
        Reader::new(
            &r,
            Limits {
                max_records: 1,
                max_output_bytes: 10_000
            }
        )
        .unwrap()
        .write_many(
            Format::Json,
            [Ok(record.clone()), Ok(record.clone())],
            &mut out
        ),
        Err(Error::TooLarge)
    ));
    let other = SchemaRef { id: 2, ..sr() };
    r.register_schema(Schema {
        reference: other,
        fields: vec![],
    })
    .unwrap();
    let other_record = r.create(1, 2, other, &[]).unwrap();
    let mut out = Vec::new();
    assert!(matches!(
        Reader::new(&r, Limits::default()).unwrap().write_many(
            Format::Csv {
                schema: sr(),
                nested: CsvNestedPolicy::Reject
            },
            [Ok(record), Ok(other_record)],
            &mut out
        ),
        Err(Error::InvalidInput(_))
    ));
}
