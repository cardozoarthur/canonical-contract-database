use canonical_core::Error;
use canonical_registry::*;
fn setup(path: &std::path::Path) -> (Registry, SchemaRef) {
    let mut r = Registry::open(path).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "test".into(),
        level: Level::Local,
    })
    .unwrap();
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: vec![Field {
            label: "text".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    (r, sr)
}
fn input(sr: SchemaRef, id: u64, text: &str) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: sr,
        values: vec![Some(Value::Utf8(text.into()))],
    }
}
#[test]
fn default_keeps_history_explicit_prune_protects_snapshot_then_reports_pruned() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    r.create(1, 1, s, &input(s, 1, &"a".repeat(4096)).values)
        .unwrap();
    let seq = r.snapshot().commit_sequence;
    let snap = r.snapshot();
    let clone = snap.clone();
    drop(snap);
    r.update(input(s, 1, "latest"), 1).unwrap();
    r.compact(Retention::KeepAll).unwrap();
    assert_eq!(r.history(1, 1).len(), 2);
    r.compact(Retention::KeepLast(1)).unwrap();
    assert!(r.revision(1, 1, 1).is_some());
    assert_eq!(clone.record(1, 1).unwrap().revision, 1);
    drop(clone);
    let report = r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(report.pruned_revisions, 1);
    assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::HistoryPruned)));
    assert!(matches!(r.snapshot_at(seq), Err(Error::HistoryPruned)));
    assert_eq!(
        r.lookup(1, 1, None)
            .unwrap()
            .revision
            .unwrap()
            .record
            .revision,
        2
    );
    r.flush_index().unwrap();
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    assert!(r.index_status().loaded);
    assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::HistoryPruned)));
    r.update(input(s, 1, "third"), 2).unwrap();
    assert_eq!(r.record(1, 1).unwrap().revision, 3);
}
#[test]
fn receipt_retry_survives_pruned_request_and_delete_previous_payload() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    let request = vec![input(s, 1, "initial")];
    let receipt = r.create_batch([1; 16], &request).unwrap();
    let update = vec![Mutation::Update {
        record: input(s, 1, "second"),
        expected_revision: 1,
    }];
    let updated = r.commit([2; 16], &update).unwrap();
    let delete = vec![Mutation::Delete {
        namespace: 1,
        id: 1,
        expected_revision: 2,
    }];
    let deleted = r.commit([3; 16], &delete).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    assert_eq!(r.create_batch([1; 16], &request).unwrap(), receipt);
    assert_eq!(r.commit([2; 16], &update).unwrap(), updated);
    assert_eq!(r.commit([3; 16], &delete).unwrap(), deleted);
    assert!(matches!(
        r.create(1, 1, s, &input(s, 1, "reuse").values),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        r.create_batch([1; 16], &[input(s, 1, "different")]),
        Err(Error::Conflict)
    ));
    r.restore(input(s, 1, "restored"), 3).unwrap();
}

#[test]
fn snapshot_lease_survives_writer_reopen_and_typed_references_are_transitive() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    r.create(1, 1, s, &input(s, 1, "old target").values)
        .unwrap();
    let reference = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference,
        fields: vec![Field {
            label: "refs".into(),
            kind: FieldType::Array {
                element: ScalarType::Ref,
                max_items: 8,
            },
            optional: false,
        }],
    })
    .unwrap();
    let refs = |id| {
        vec![Some(Value::Array(vec![ScalarValue::Ref(Reference {
            namespace: 1,
            id,
        })]))]
    };
    r.create(1, 2, reference, &refs(1)).unwrap();
    r.create(1, 3, reference, &refs(2)).unwrap();
    let snap = r.snapshot();
    let seq = snap.commit_sequence;
    r.update(input(s, 1, "latest target"), 1).unwrap();
    r.update(
        NewRecord {
            namespace: 1,
            id: 2,
            schema: reference,
            values: vec![Some(Value::Array(vec![]))],
        },
        1,
    )
    .unwrap();
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert!(r.revision(1, 1, 1).is_some());
    assert!(r.revision(1, 2, 1).is_some());
    drop(snap);
    r.compact(Retention::KeepLast(1)).unwrap();
    // 3@1 pins 2@1, which transitively pins 1@1; not just current endpoint heads.
    assert!(r.revision(1, 1, 1).is_some());
    assert!(r.revision(1, 2, 1).is_some());
    assert_eq!(r.snapshot_at(seq).unwrap().len(), 3);
    r.delete(1, 3, 1).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert!(r.revision(1, 1, 1).is_none());
    assert!(r.revision(1, 2, 1).is_none());
}
#[test]
fn native_edges_and_typed_struct_reference_history_remain_readable() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    r.create(1, 1, s, &input(s, 1, "source old").values)
        .unwrap();
    r.create(1, 2, s, &input(s, 2, "target old").values)
        .unwrap();
    let edge = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
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
        source_schema: s,
        target_schema: s,
    })
    .unwrap();
    let values = vec![Some(Value::Relation {
        source: Reference {
            namespace: 1,
            id: 1,
        },
        kind: 1,
        target: Reference {
            namespace: 1,
            id: 2,
        },
    })];
    r.create(1, 3, edge, &values).unwrap();
    r.update(input(s, 1, "source new"), 1).unwrap();
    r.update(input(s, 2, "target new"), 1).unwrap();
    r.delete(1, 3, 1).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(r.history(1, 3).len(), 2);
    assert!(r.revision(1, 1, 1).is_some());
    assert!(r.revision(1, 2, 1).is_some());
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    assert_eq!(
        r.lookup(1, 3, Some(1))
            .unwrap()
            .revision
            .unwrap()
            .record
            .schema,
        edge
    );
    r.restore(
        NewRecord {
            namespace: 1,
            id: 3,
            schema: edge,
            values,
        },
        2,
    )
    .unwrap();
    assert!(matches!(r.delete(1, 1, 2), Err(Error::Conflict)));
}
#[test]
fn pruned_schema_usage_still_prevents_later_native_reinterpretation() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    r.create(1, 1, s, &input(s, 1, "node").values).unwrap();
    let edge = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference: edge,
        fields: vec![Field {
            label: "opaque edge".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.create(
        1,
        3,
        edge,
        &[Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 1,
            target: Reference {
                namespace: 1,
                id: 1,
            },
        })],
    )
    .unwrap();
    r.update(input(s, 3, "now ordinary"), 1).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(r.revision_status(1, 3, 1), RevisionStatus::Pruned);
    assert!(matches!(
        r.register_relation_rule(RelationRule {
            schema: edge,
            kind: 1,
            source_schema: s,
            target_schema: s
        }),
        Err(Error::Conflict)
    ));
}
#[test]
fn compaction_keeps_atomic_member_ordinals_and_append_sequences() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    let batch = vec![input(s, 1, "one"), input(s, 2, "two"), input(s, 3, "three")];
    let receipt = r.create_batch([1; 16], &batch).unwrap();
    r.update(input(s, 1, "new one"), 1).unwrap();
    r.update(input(s, 2, "new two"), 1).unwrap();
    let seq = r.snapshot().commit_sequence;
    let report = r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(report.pruned_revisions, 2);
    let found = r.lookup(1, 3, Some(1)).unwrap();
    assert_eq!(found.metrics.decoded_members, 1);
    assert_eq!(found.metrics.point_reads, 1);
    let detailed = r
        .lookup_many(
            &[LookupRequest {
                namespace: 1,
                id: 3,
                revision: Some(1),
            }],
            HistoryReadMode::Selective,
        )
        .unwrap();
    assert_eq!(detailed.metrics.members_visited, 3); // original physical ordinal remains after pruned slots
    let reference = r
        .lookup_many(
            &[LookupRequest {
                namespace: 1,
                id: 3,
                revision: Some(1),
            }],
            HistoryReadMode::ReferenceFull,
        )
        .unwrap();
    assert_eq!(reference.results[0].revision, found.revision);
    assert_eq!(reference.metrics.members_decoded, 3);
    assert_eq!(
        found.revision.unwrap().commit_sequence,
        receipt.commit_sequence
    );
    assert_eq!(r.create_batch([1; 16], &batch).unwrap(), receipt);
    r.update(input(s, 3, "new three"), 1).unwrap();
    assert_eq!(r.snapshot().commit_sequence, seq + 1);
    let cleaned = r.cleanup_obsolete_generations().unwrap();
    assert_eq!(cleaned.removed_generations, vec![1]);
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    assert_eq!(
        r.lookup(1, 3, None)
            .unwrap()
            .revision
            .unwrap()
            .record
            .revision,
        2
    );
}
#[test]
fn invalid_retention_is_rejected_before_write_and_empty_database_compacts() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    assert!(matches!(
        r.compact(Retention::KeepLast(0)),
        Err(Error::InvalidInput(_))
    ));
    let report = r.compact(Retention::default()).unwrap();
    assert_eq!(report.retained_revisions, 0);
    drop(r);
    assert_eq!(Registry::open(d.path()).unwrap().snapshot().len(), 0);
}

#[test]
fn nested_struct_refs_pin_historical_target_and_cycles_terminate() {
    let d = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(d.path());
    r.create(1, 1, s, &input(s, 1, "original").values).unwrap();
    let child = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference: child,
        fields: vec![Field {
            label: "ref".into(),
            kind: FieldType::Ref,
            optional: false,
        }],
    })
    .unwrap();
    let outer = SchemaRef {
        namespace: 1,
        id: 3,
        version: 1,
    };
    r.register_schema(Schema {
        reference: outer,
        fields: vec![Field {
            label: "nested".into(),
            kind: FieldType::Struct(child),
            optional: false,
        }],
    })
    .unwrap();
    let bytes = r
        .encode_values(
            child,
            &[Some(Value::Ref(Reference {
                namespace: 1,
                id: 1,
            }))],
        )
        .unwrap();
    r.create(1, 2, outer, &[Some(Value::Struct(bytes))])
        .unwrap();
    r.update(input(s, 1, "new"), 1).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    assert!(r.revision(1, 1, 1).is_some());
    // Self-reference and a two-node cycle in a single atomic transaction are finite.
    let first = NewRecord {
        namespace: 1,
        id: 3,
        schema: child,
        values: vec![Some(Value::Ref(Reference {
            namespace: 1,
            id: 4,
        }))],
    };
    let second = NewRecord {
        namespace: 1,
        id: 4,
        schema: child,
        values: vec![Some(Value::Ref(Reference {
            namespace: 1,
            id: 3,
        }))],
    };
    r.create_batch([9; 16], &[first, second]).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    drop(r);
    assert_eq!(Registry::open(d.path()).unwrap().snapshot().len(), 4);
}

#[test]
fn logical_recovery_at_each_publication_stage_keeps_heads_and_receipts() {
    use canonical_storage::RewriteStage;
    for stage in [
        RewriteStage::Prepared,
        RewriteStage::SegmentWritten,
        RewriteStage::SegmentSynced,
        RewriteStage::ManifestWritten,
        RewriteStage::ManifestPublished,
        RewriteStage::Activated,
    ] {
        let d = tempfile::tempdir().unwrap();
        let (mut r, s) = setup(d.path());
        let first = vec![input(s, 1, "first")];
        let receipt = r.create_batch([1; 16], &first).unwrap();
        r.update(input(s, 1, "second"), 1).unwrap();
        let seq = r.snapshot().commit_sequence;
        drop(r);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "registry_compaction_child", "--ignored"])
            .env("CBR_REGISTRY_COMPACT_DIR", d.path())
            .env("CBR_REGISTRY_COMPACT_STAGE", format!("{stage:?}"))
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(74));
        let mut r = Registry::open(d.path()).unwrap();
        assert_eq!(r.snapshot().commit_sequence, seq);
        assert_eq!(r.record(1, 1).unwrap().revision, 2);
        assert_eq!(r.create_batch([1; 16], &first).unwrap(), receipt);
        let pruned = matches!(
            stage,
            RewriteStage::ManifestPublished | RewriteStage::Activated
        );
        assert_eq!(
            r.revision_status(1, 1, 1),
            if pruned {
                RevisionStatus::Pruned
            } else {
                RevisionStatus::Available
            }
        );
        r.update(input(s, 1, "third"), 2).unwrap();
        assert_eq!(r.snapshot().commit_sequence, seq + 1);
    }
}
#[test]
#[ignore = "invoked by parent crash test only"]
fn registry_compaction_child() {
    let path = std::env::var_os("CBR_REGISTRY_COMPACT_DIR").unwrap();
    let stage = std::env::var("CBR_REGISTRY_COMPACT_STAGE").unwrap();
    let mut r = Registry::open(path).unwrap();
    r.compact_with_hook(Retention::KeepLast(1), |seen| {
        if format!("{seen:?}") == stage {
            std::process::exit(74);
        }
        Ok(())
    })
    .unwrap();
    panic!("failpoint not reached");
}
#[test]
fn closed_backup_before_compaction_restores_full_history_and_receipts() {
    let original = tempfile::tempdir().unwrap();
    let (mut r, s) = setup(original.path());
    let batch = vec![input(s, 1, "before")];
    let receipt = r.create_batch([1; 16], &batch).unwrap();
    r.update(input(s, 1, "current"), 1).unwrap();
    r.checkpoint().unwrap();
    drop(r);
    let backup = tempfile::tempdir().unwrap();
    std::fs::create_dir(backup.path().join("segments")).unwrap();
    for name in ["manifest.cbr", "segments/00000001.cbr", "checkpoint.cbr"] {
        std::fs::copy(original.path().join(name), backup.path().join(name)).unwrap();
    }
    let mut r = Registry::open(original.path()).unwrap();
    r.compact(Retention::KeepLast(1)).unwrap();
    r.cleanup_obsolete_generations().unwrap();
    drop(r);
    let mut restored = Registry::open(backup.path()).unwrap();
    assert_eq!(restored.history(1, 1).len(), 2);
    assert_eq!(restored.create_batch([1; 16], &batch).unwrap(), receipt);
    assert_eq!(restored.record(1, 1).unwrap().revision, 2);
    restored.update(input(s, 1, "after restore"), 2).unwrap();
}
