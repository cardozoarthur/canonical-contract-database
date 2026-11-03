use canonical_registry::*;

#[test]
fn compact_timeline_preserves_namespace_identity_full_history_and_every_commit_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    let mut snapshots = Vec::new();
    for ns in [1, 2] {
        r.register_namespace(Namespace {
            id: ns,
            name: format!("ns{ns}"),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: ns,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        r.create(ns, 7, sr, &[Some(Value::Bytes(vec![ns as u8; 4096]))])
            .unwrap();
        snapshots.push(r.snapshot());
        let v2 = SchemaRef { version: 2, ..sr };
        r.register_schema(Schema {
            reference: v2,
            fields: vec![
                Field {
                    label: "bytes".into(),
                    kind: FieldType::Bytes,
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
        let input = NewRecord {
            namespace: ns,
            id: 7,
            schema: v2,
            values: vec![Some(Value::Bytes(vec![9; 33])), None],
        };
        r.update(input.clone(), 1).unwrap();
        snapshots.push(r.snapshot());
        let tomb = r.delete(ns, 7, 2).unwrap();
        assert_eq!(tomb.record.schema, v2);
        assert!(tomb.record.payload.is_empty());
        snapshots.push(r.snapshot());
        r.restore(input, 3).unwrap();
        snapshots.push(r.snapshot());
    }
    let histories: Vec<_> = [1, 2].iter().map(|ns| r.history(*ns, 7).to_vec()).collect();
    drop(r);
    let r = Registry::open(dir.path()).unwrap();
    for (ns, expected) in [1, 2].iter().zip(histories) {
        assert_eq!(r.history(*ns, 7), expected);
    }
    for old in snapshots {
        let actual = r.snapshot_at(old.commit_sequence).unwrap();
        for ns in [1, 2] {
            assert_eq!(actual.record(ns, 7), old.record(ns, 7));
        }
    }
}
