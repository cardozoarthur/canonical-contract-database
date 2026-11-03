use canonical_registry::*;
use canonical_storage::Store;
use serde::Serialize;
use std::{fs, panic::catch_unwind};

fn schema_ref() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}

fn setup(path: &std::path::Path) -> Registry {
    let mut registry = Registry::open(path).unwrap();
    registry
        .register_namespace(Namespace {
            id: 1,
            name: "corpus".into(),
            level: Level::Local,
        })
        .unwrap();
    registry
        .register_schema(Schema {
            reference: schema_ref(),
            fields: vec![Field {
                label: "value".into(),
                kind: FieldType::UInt,
                optional: false,
            }],
        })
        .unwrap();
    registry
}

// Public v1 serialization contract: positional struct + enum tag. The single
// variant is enough to construct invalid namespace facts with valid physical CRC.
#[derive(Serialize)]
enum NamespaceEvent {
    Namespace(Namespace),
}
#[derive(Serialize)]
struct Envelope {
    version: u16,
    event: NamespaceEvent,
}

// Numeric enum tags intentionally mirror the append-only v1 wire contract.
// Serializing this tuple yields Envelope(version, Event::Compacted(...)).
#[derive(Clone, Serialize)]
struct WireChange {
    kind: u32,
    expected_revision: u64,
    record: Record,
}
#[derive(Clone, Serialize)]
struct WireMember {
    change: WireChange,
    pruned: bool,
}
#[derive(Clone, Serialize)]
struct WireCompacted {
    version: u16,
    kind: u32,
    tx_id: Option<[u8; 16]>,
    fingerprint: Option<[u8; 32]>,
    members: Vec<WireMember>,
}

#[test]
fn compacted_envelope_rejects_malformed_shapes_and_a_pruned_current_head() {
    let source = tempfile::tempdir().unwrap();
    let registry = setup(source.path());
    let payload = registry
        .encode_values(schema_ref(), &[Some(Value::UInt(42))])
        .unwrap();
    drop(registry);
    let valid = WireCompacted {
        version: 1,
        kind: 0,
        tx_id: None,
        fingerprint: None,
        members: vec![WireMember {
            change: WireChange {
                kind: 0,
                expected_revision: 0,
                record: Record {
                    namespace: 1,
                    id: 8,
                    schema: schema_ref(),
                    revision: 1,
                    payload,
                },
            },
            pruned: false,
        }],
    };
    // A control proves that the synthetic wire representation really matches
    // Event variant 8 rather than rejecting every case due to a fixture error.
    let mut store = Store::open(source.path()).unwrap();
    store
        .append(&canonical_codec::encode(&(1u16, 8u32, &valid)).unwrap())
        .unwrap();
    drop(store);
    assert_eq!(
        Registry::open(source.path())
            .unwrap()
            .decode_record(1, 8)
            .unwrap(),
        vec![Some(Value::UInt(42))]
    );

    let mut cases = Vec::new();
    let mut case = valid.clone();
    case.version = 2;
    cases.push(case);
    let mut case = valid.clone();
    case.kind = 99;
    cases.push(case);
    let mut case = valid.clone();
    case.members.clear();
    cases.push(case);
    let mut case = valid.clone();
    case.tx_id = Some([1; 16]);
    cases.push(case);
    let mut case = valid.clone();
    case.fingerprint = Some([1; 32]);
    cases.push(case);
    let mut case = valid.clone();
    case.kind = 1;
    cases.push(case);
    let mut case = valid.clone();
    case.members.push(case.members[0].clone());
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].pruned = true;
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].pruned = true;
    case.members[0].change.record.payload.clear();
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].change.record.revision = 0;
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].change.expected_revision = 7;
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].change.record.schema.id = 999;
    cases.push(case);
    let mut case = valid.clone();
    case.members[0].change.kind = 2;
    cases.push(case);
    for (index, case) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        drop(setup(dir.path()));
        let mut store = Store::open(dir.path()).unwrap();
        store
            .append(&canonical_codec::encode(&(1u16, 8u32, case)).unwrap())
            .unwrap();
        drop(store);
        let path = dir.path().join("segments/00000001.cbr");
        let before = fs::read(&path).unwrap();
        assert!(
            catch_unwind(|| Registry::open(dir.path()))
                .unwrap()
                .is_err(),
            "accepted compacted case {index}"
        );
        assert!(
            catch_unwind(|| Registry::recover(dir.path()))
                .unwrap()
                .is_err(),
            "recovered compacted case {index}"
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
fn crc_valid_invalid_logical_events_are_rejected_without_modifying_log() {
    let cases = vec![
        vec![],
        vec![1],
        vec![1, 127],
        vec![1, 255, 255, 255, 255, 15],
        canonical_codec::encode(&Envelope {
            version: 2,
            event: NamespaceEvent::Namespace(Namespace {
                id: 2,
                name: "valid-name".into(),
                level: Level::Local,
            }),
        })
        .unwrap(),
        canonical_codec::encode(&Envelope {
            version: 1,
            event: NamespaceEvent::Namespace(Namespace {
                id: 1,
                name: "duplicate-id".into(),
                level: Level::Local,
            }),
        })
        .unwrap(),
        canonical_codec::encode(&Envelope {
            version: 1,
            event: NamespaceEvent::Namespace(Namespace {
                id: 2,
                name: "".into(),
                level: Level::Local,
            }),
        })
        .unwrap(),
    ];
    for payload in cases {
        let dir = tempfile::tempdir().unwrap();
        drop(setup(dir.path()));
        let mut store = Store::open(dir.path()).unwrap();
        store.append(&payload).unwrap();
        drop(store);
        let path = dir.path().join("segments/00000001.cbr");
        let before = fs::read(&path).unwrap();
        assert!(
            catch_unwind(|| Registry::open(dir.path()))
                .unwrap()
                .is_err()
        );
        assert!(
            catch_unwind(|| Registry::recover(dir.path()))
                .unwrap()
                .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[test]
fn typed_payload_cut_mutation_and_hostile_lengths_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let registry = setup(dir.path());
    let valid = registry
        .encode_values(schema_ref(), &[Some(Value::UInt(42))])
        .unwrap();
    for cut in 0..valid.len() {
        assert!(
            catch_unwind(|| registry.decode_values(schema_ref(), &valid[..cut]))
                .unwrap()
                .is_err()
        );
    }
    for offset in 0..valid.len() {
        for bit in 0..8 {
            let mut changed = valid.clone();
            changed[offset] ^= 1 << bit;
            if let Ok(values) =
                catch_unwind(|| registry.decode_values(schema_ref(), &changed)).unwrap()
            {
                // A payload itself is not checksummed. Legitimate changed UInt
                // values are allowed; malformed values/types must be rejected.
                assert!(matches!(values.as_slice(), [Some(Value::UInt(_))]));
            }
        }
    }
    for bytes in [
        vec![0xff; 64],
        vec![0, 0xfe, 0xff, 0xff, 0xff, 15],
        vec![0, 1, 9, 0xfe, 0xff, 0xff, 0xff, 15],
    ] {
        assert!(
            catch_unwind(|| registry.decode_values(schema_ref(), &bytes))
                .unwrap()
                .is_err()
        );
    }
}

#[cfg(windows)]
#[test]
fn snapshot_pin_survives_windows_case_alias_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mixed = dir.path().join("MixedCaseDatabase");
    let mut registry = setup(&mixed);
    registry
        .create(1, 1, schema_ref(), &[Some(Value::UInt(1))])
        .unwrap();
    let snapshot = registry.snapshot();
    registry
        .update(
            NewRecord {
                namespace: 1,
                id: 1,
                schema: schema_ref(),
                values: vec![Some(Value::UInt(2))],
            },
            1,
        )
        .unwrap();
    drop(registry);
    let alias = dir.path().join("mixedcasedatabase");
    let mut registry = Registry::open(alias).unwrap();
    registry.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(snapshot.record(1, 1).unwrap().revision, 1);
    assert_eq!(registry.revision_status(1, 1, 1), RevisionStatus::Available);
}

#[test]
fn near_limit_atomic_frame_retains_extra_member_when_wrapper_cannot_fit() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = setup(dir.path());
    let large_schema = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    registry
        .register_schema(Schema {
            reference: large_schema,
            fields: (0..16)
                .map(|n| Field {
                    label: format!("bytes{n}"),
                    kind: FieldType::Bytes,
                    optional: false,
                })
                .collect(),
        })
        .unwrap();
    let mut values: Vec<_> = (0..16)
        .map(|_| Some(Value::Bytes(vec![7; MAX_TEXT_BYTES])))
        .collect();
    let Some(Value::Bytes(last)) = &mut values[15] else {
        unreachable!()
    };
    last.truncate(MAX_TEXT_BYTES - 500);
    let small_values = vec![Some(Value::UInt(42))];
    let mut large_record = Record {
        namespace: 1,
        id: 1,
        schema: large_schema,
        revision: 1,
        payload: registry.encode_values(large_schema, &values).unwrap(),
    };
    let small_record = Record {
        namespace: 1,
        id: 2,
        schema: schema_ref(),
        revision: 1,
        payload: registry.encode_values(schema_ref(), &small_values).unwrap(),
    };
    let maximum_payload = canonical_core::MAX_FRAME_BYTES - canonical_codec::FRAME_OVERHEAD;
    let used = 32
        + canonical_codec::encode(&large_record).unwrap().len()
        + canonical_codec::encode(&small_record).unwrap().len();
    let extra = maximum_payload - used;
    let Some(Value::Bytes(last)) = &mut values[15] else {
        unreachable!()
    };
    last.resize(last.len() + extra, 7);
    assert!(last.len() <= MAX_TEXT_BYTES);
    large_record.payload = registry.encode_values(large_schema, &values).unwrap();
    // Prove this input hits wrapper overflow, rather than merely testing a large
    // frame that compacted successfully through the ordinary path.
    let wrapper = WireCompacted {
        version: 1,
        kind: 1,
        tx_id: Some([9; 16]),
        fingerprint: Some([0; 32]),
        members: vec![
            WireMember {
                change: WireChange {
                    kind: 0,
                    expected_revision: 0,
                    record: large_record,
                },
                pruned: false,
            },
            WireMember {
                change: WireChange {
                    kind: 0,
                    expected_revision: 0,
                    record: Record {
                        payload: vec![],
                        ..small_record
                    },
                },
                pruned: true,
            },
        ],
    };
    assert!(matches!(
        canonical_codec::encode(&(1u16, 8u32, wrapper)),
        Err(canonical_core::Error::TooLarge)
    ));
    registry
        .create_batch(
            [9; 16],
            &[
                NewRecord {
                    namespace: 1,
                    id: 1,
                    schema: large_schema,
                    values,
                },
                NewRecord {
                    namespace: 1,
                    id: 2,
                    schema: schema_ref(),
                    values: small_values,
                },
            ],
        )
        .unwrap();
    registry
        .update(
            NewRecord {
                namespace: 1,
                id: 2,
                schema: schema_ref(),
                values: vec![Some(Value::UInt(43))],
            },
            1,
        )
        .unwrap();
    let report = registry.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(report.pruned_revisions, 0);
    assert_eq!(registry.revision_status(1, 2, 1), RevisionStatus::Available);
    registry.flush_index().unwrap();
    drop(registry);
    let mut registry = Registry::open(dir.path()).unwrap();
    assert!(registry.index_status().loaded);
    assert_eq!(
        registry
            .lookup(1, 2, Some(1))
            .unwrap()
            .revision
            .unwrap()
            .record
            .revision,
        1
    );
}
