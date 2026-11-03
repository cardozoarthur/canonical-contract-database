use canonical_registry::*;
fn fixture() -> (
    tempfile::TempDir,
    Registry,
    SchemaRef,
    Vec<Option<Value>>,
    Vec<u8>,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 7,
        name: "projection".into(),
        level: Level::Local,
    })
    .unwrap();
    let inner = SchemaRef {
        namespace: 7,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference: inner,
        fields: vec![Field {
            label: "inner".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    let sr = SchemaRef {
        namespace: 7,
        id: 1,
        version: 1,
    };
    let kinds = vec![
        FieldType::Bool,
        FieldType::UInt,
        FieldType::Int,
        FieldType::Decimal { scale: 2 },
        FieldType::Float,
        FieldType::Enum {
            variants: vec![3, 5],
        },
        FieldType::Ref,
        FieldType::Utf8,
        FieldType::Bytes,
        FieldType::Array {
            element: ScalarType::Utf8,
            max_items: 65536,
        },
        FieldType::Struct(inner),
        FieldType::Relation,
        FieldType::FixedInt,
        FieldType::UInt,
    ];
    r.register_schema(Schema {
        reference: sr,
        fields: kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| Field {
                label: format!("f{i}"),
                kind,
                optional: i == 13,
            })
            .collect(),
    })
    .unwrap();
    let nested = r
        .encode_values(inner, &[Some(Value::Utf8("z".repeat(65536)))])
        .unwrap();
    let vals = vec![
        Some(Value::Bool(true)),
        Some(Value::UInt(128)),
        Some(Value::Int(-73)),
        Some(Value::Decimal {
            mantissa: -123,
            scale: 2,
        }),
        Some(Value::Float(2.5)),
        Some(Value::Enum(3)),
        Some(Value::Ref(Reference {
            namespace: 7,
            id: 9,
        })),
        Some(Value::Utf8("a".repeat(65536))),
        Some(Value::Bytes(vec![4; 65536])),
        Some(Value::Array(vec![ScalarValue::Utf8("b".repeat(4096)); 16])),
        Some(Value::Struct(nested)),
        Some(Value::Relation {
            source: Reference {
                namespace: 7,
                id: 9,
            },
            kind: 2,
            target: Reference {
                namespace: 7,
                id: 10,
            },
        }),
        Some(Value::FixedInt(-123456789)),
        None,
    ];
    let payload = r.encode_values(sr, &vals).unwrap();
    (dir, r, sr, vals, payload)
}
#[test]
fn selected_all_types_order_optional_and_detached_catalog() {
    let (_dir, r, sr, values, payload) = fixture();
    let decoder = r.projection_decoder();
    assert_eq!(decoder.schemas().count(), 2);
    assert_eq!(decoder.schema(sr).unwrap().reference, sr);
    drop(r);
    for (position, value) in values.iter().enumerate() {
        assert_eq!(
            decoder.decode_selected(sr, &payload, &[position]).unwrap(),
            vec![value.clone()]
        );
    }
    assert_eq!(
        decoder.decode_selected(sr, &payload, &[12, 1, 13]).unwrap(),
        vec![values[12].clone(), values[1].clone(), None]
    );
    assert!(
        decoder
            .decode_selected(sr, &payload, &[])
            .unwrap()
            .is_empty()
    );
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static ALLOCATED: Cell<(usize,usize)> = const { Cell::new((0,0)) };
}
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        TRACK.with(|t| {
            if t.get() {
                ALLOCATED.with(|a| {
                    let (c, b) = a.get();
                    a.set((c + 1, b + l.size()));
                });
            }
        });
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        TRACK.with(|t| {
            if t.get() {
                ALLOCATED.with(|a| {
                    let (c, b) = a.get();
                    a.set((c + 1, b + n));
                });
            }
        });
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn allocated<T>(f: impl FnOnce() -> T) -> (T, (usize, usize)) {
    ALLOCATED.with(|a| a.set((0, 0)));
    TRACK.with(|t| t.set(true));
    let out = f();
    TRACK.with(|t| t.set(false));
    (out, ALLOCATED.with(Cell::get))
}
#[test]
fn omitted_text_bytes_array_and_nested_struct_allocate_no_values() {
    let (_dir, r, sr, _, payload) = fixture();
    let decoder = r.projection_decoder();
    let (out, cost) = allocated(|| decoder.decode_selected(sr, &payload, &[]).unwrap());
    assert!(out.is_empty());
    assert_eq!(cost, (0, 0), "validation-only must allocate nothing");
    let (out, cost) = allocated(|| decoder.decode_selected(sr, &payload, &[1]).unwrap());
    assert_eq!(out, vec![Some(Value::UInt(128))]);
    assert_eq!(cost, (1, std::mem::size_of::<Option<Value>>()));
    let (out, cost) = allocated(|| decoder.decode_selected(sr, &payload, &[7]).unwrap());
    assert!(matches!(&out[0],Some(Value::Utf8(s)) if s.len()==65536));
    assert_eq!(cost, (2, std::mem::size_of::<Option<Value>>() + 65536));
}
#[derive(serde::Serialize)]
struct WirePayload {
    bitmap: Vec<u8>,
    values: Vec<Value>,
}
fn raw(values: Vec<Option<Value>>) -> Vec<u8> {
    canonical_codec::encode(&WirePayload {
        bitmap: vec![u8::from(values[13].is_some())],
        values: values.into_iter().flatten().collect(),
    })
    .unwrap()
}
#[test]
fn omitted_invalid_fields_are_validated_like_full_decode() {
    let (_dir, r, sr, values, payload) = fixture();
    let d = r.projection_decoder();
    let mut bad = Vec::new();
    for (pos, value) in [
        (0, Value::UInt(1)),
        (
            3,
            Value::Decimal {
                mantissa: 1,
                scale: 3,
            },
        ),
        (4, Value::Float(f64::NAN)),
        (5, Value::Enum(9)),
        (
            6,
            Value::Ref(Reference {
                namespace: 777,
                id: 1,
            }),
        ),
        (7, Value::Utf8("a".repeat(MAX_TEXT_BYTES + 1))),
        (8, Value::Bytes(vec![0; MAX_TEXT_BYTES + 1])),
        (9, Value::Array(vec![ScalarValue::Bool(true)])),
        (
            9,
            Value::Array(vec![ScalarValue::Utf8(String::new()); MAX_ARRAY + 1]),
        ),
        (10, Value::Struct(vec![0])),
        (
            11,
            Value::Relation {
                source: Reference {
                    namespace: 7,
                    id: 0,
                },
                kind: 1,
                target: Reference {
                    namespace: 7,
                    id: 1,
                },
            },
        ),
    ] {
        let mut v = values.clone();
        v[pos] = Some(value);
        bad.push(raw(v));
    }
    let mut trailing = payload.clone();
    trailing.push(0);
    bad.push(trailing);
    for length in [0, 1, 2, payload.len() - 1] {
        bad.push(payload[..length].to_vec());
    }
    let mut reserved = payload.clone();
    reserved[1] = 128;
    bad.push(reserved);
    let mut missing = values.clone();
    missing[0] = None;
    bad.push(raw(missing));
    let mut excess = values.clone();
    excess.push(Some(Value::UInt(1)));
    bad.push(raw(excess));
    // Unknown top-level enum discriminant: bitmap length, bitmap, values length, first tag.
    let mut tag = payload.clone();
    tag[3] = 127;
    bad.push(tag);
    for (i, b) in bad.iter().enumerate() {
        assert!(
            r.decode_values(sr, b).is_err(),
            "full oracle accepted bad {i}"
        );
        assert!(
            d.decode_selected(sr, b, &[]).is_err(),
            "skip accepted bad {i}"
        );
        assert!(
            d.decode_selected(sr, b, &[1]).is_err(),
            "selection accepted bad {i}"
        );
    }
    assert!(matches!(
        d.decode_selected(sr, &payload, &[0, 0]),
        Err(canonical_core::Error::InvalidInput(_))
    ));
    assert!(matches!(
        d.decode_selected(sr, &payload, &[99]),
        Err(canonical_core::Error::InvalidInput(_))
    ));
    assert!(matches!(
        d.decode_selected(SchemaRef { version: 99, ..sr }, &payload, &[]),
        Err(canonical_core::Error::UnknownSchema)
    ));
    assert!(matches!(
        d.decode_selected(sr, &payload, &vec![0; MAX_FIELDS + 1]),
        Err(canonical_core::Error::TooLarge)
    ));
    assert!(matches!(
        d.decode_selected(sr, &vec![0; canonical_core::MAX_FRAME_BYTES], &[]),
        Err(canonical_core::Error::TooLarge)
    ));
}
#[test]
fn scalar_arrays_and_global_namespace_references_preserve_types() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 0,
        name: "global".into(),
        level: Level::Global,
    })
    .unwrap();
    let scalar = vec![
        (ScalarType::Bool, ScalarValue::Bool(true)),
        (ScalarType::UInt, ScalarValue::UInt(128)),
        (ScalarType::Int, ScalarValue::Int(-99)),
        (ScalarType::Utf8, ScalarValue::Utf8("é".into())),
        (ScalarType::Bytes, ScalarValue::Bytes(vec![1, 128])),
        (
            ScalarType::Ref,
            ScalarValue::Ref(Reference {
                namespace: 0,
                id: 1,
            }),
        ),
    ];
    let sr = SchemaRef {
        namespace: 0,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: scalar
            .iter()
            .enumerate()
            .map(|(i, (kind, _))| Field {
                label: format!("a{i}"),
                kind: FieldType::Array {
                    element: kind.clone(),
                    max_items: 2,
                },
                optional: false,
            })
            .collect(),
    })
    .unwrap();
    let values: Vec<_> = scalar
        .into_iter()
        .map(|(_, v)| Some(Value::Array(vec![v])))
        .collect();
    let bytes = r.encode_values(sr, &values).unwrap();
    let d = r.projection_decoder();
    assert_eq!(
        d.decode_selected(sr, &bytes, &[0, 1, 2, 3, 4, 5]).unwrap(),
        values
    );
    assert!(d.decode_selected(sr, &bytes, &[]).unwrap().is_empty());
}
#[test]
fn nested_depth_limit_and_catalog_freeze_match_complete_decoder() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 0,
        name: "depth".into(),
        level: Level::Global,
    })
    .unwrap();
    let mut payload = canonical_codec::encode(&WirePayload {
        bitmap: vec![],
        values: vec![Value::UInt(1)],
    })
    .unwrap();
    let mut previous = SchemaRef {
        namespace: 0,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: previous,
        fields: vec![Field {
            label: "leaf".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    let frozen = r.projection_decoder();
    for depth in 1..=MAX_DEPTH + 1 {
        let sr = SchemaRef {
            id: depth as u64 + 1,
            ..previous
        };
        let registered = r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "nested".into(),
                kind: FieldType::Struct(previous),
                optional: false,
            }],
        });
        if depth > MAX_DEPTH {
            assert!(registered.is_err());
            break;
        }
        registered.unwrap();
        payload = canonical_codec::encode(&WirePayload {
            bitmap: vec![],
            values: vec![Value::Struct(payload)],
        })
        .unwrap();
        let decoder = r.projection_decoder();
        let full = r.decode_values(sr, &payload);
        let skip = decoder.decode_selected(sr, &payload, &[]);
        assert_eq!(full.is_ok(), depth <= MAX_DEPTH);
        assert_eq!(skip.is_ok(), full.is_ok());
        if depth <= MAX_DEPTH {
            assert_eq!(
                decoder.decode_selected(sr, &payload, &[0]).unwrap(),
                full.unwrap()
            );
        }
        previous = sr;
    }
    assert_eq!(frozen.schemas().count(), 1);
    assert!(matches!(
        frozen.schema(previous),
        Err(canonical_core::Error::UnknownSchema)
    ));
}
