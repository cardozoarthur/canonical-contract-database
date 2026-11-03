//! State-owned append-only binding of complete schema references; never serialized.
use super::*;
impl State {
    pub(super) fn metadata_schema(&self, meta: &HistoryMeta) -> Option<SchemaRef> {
        self.schema_table.get(meta.schema_ordinal).copied()
    }
    pub(super) fn validate_schema_table(&self) -> Result<()> {
        if self.schema_table.len() != self.schema_ordinals.len()
            || self.schema_table.len() != self.schemas.len()
        {
            return Err(invalid("schema binding cardinality mismatch"));
        }
        for (ordinal, reference) in self.schema_table.iter().enumerate() {
            if self.schema_ordinals.get(reference) != Some(&ordinal)
                || !self.schemas.contains_key(reference)
            {
                return Err(invalid("invalid schema binding"));
            }
        }
        Ok(())
    }
    pub(super) fn validate_schema_bindings(&self, event: &Event) -> Result<()> {
        if let Event::Schema(schema) = event {
            if self.schemas.contains_key(&schema.reference)
                || self.schema_ordinals.contains_key(&schema.reference)
            {
                return Err(Error::Conflict);
            }
            self.schema_table
                .len()
                .checked_add(1)
                .ok_or(Error::TooLarge)?;
        }
        for (record, _) in event_members(event) {
            let ordinal = self
                .schema_ordinals
                .get(&record.schema)
                .ok_or(Error::UnknownSchema)?;
            if self.schema_table.get(*ordinal) != Some(&record.schema)
                || !self.schemas.contains_key(&record.schema)
            {
                return Err(invalid("record schema binding mismatch"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup(path: &Path) -> (Registry, SchemaRef) {
        let mut r = Registry::open(path).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "schemas".into(),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 5,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![],
        })
        .unwrap();
        (r, sr)
    }
    fn schema(reference: SchemaRef) -> Schema {
        Schema {
            reference,
            fields: vec![],
        }
    }
    #[test]
    fn bindings_append_preserve_old_versions_pruned_slots_replay_and_failed_rewrite_prefix() {
        for hot in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let (mut r, sr1) = setup(d.path());
            let first = r.create(1, 1, sr1, &[]).unwrap();
            let seq1 = r.sequence;
            let ordinal = r.state.history_meta[&(1, 1)][0].schema_ordinal;
            let before = r.state.schema_table.clone();
            let sequence = r.sequence;
            r.register_schema(schema(sr1)).unwrap();
            assert_eq!(r.sequence, sequence);
            assert_eq!(r.state.schema_table, before);
            if hot {
                assert_eq!(r.history(1, 1)[0].record, first);
            }
            let sr2 = SchemaRef { version: 2, ..sr1 };
            r.register_schema(schema(sr2)).unwrap();
            let other = SchemaRef {
                id: 2,
                version: 1,
                ..sr1
            };
            r.register_schema(schema(other)).unwrap();
            assert_eq!(r.state.schema_table[ordinal], sr1);
            assert_eq!(r.state.history_meta[&(1, 1)][0].schema_ordinal, ordinal);
            let second = r
                .update(
                    NewRecord {
                        namespace: 1,
                        id: 1,
                        schema: sr2,
                        values: vec![],
                    },
                    1,
                )
                .unwrap();
            let seq2 = r.sequence;
            let expected = vec![
                Revision {
                    record: first,
                    deleted: false,
                    commit_sequence: seq1,
                },
                Revision {
                    record: second,
                    deleted: false,
                    commit_sequence: seq2,
                },
            ];
            let table = r.state.schema_table.clone();
            let bindings = r.state.schema_ordinals.clone();
            if hot {
                assert_eq!(r.history(1, 1), expected);
            } else {
                assert!(r.state.history[&(1, 1)].get().is_none());
            }
            assert!(
                r.compact_with_hook(Retention::KeepLast(1), |s| {
                    if s == canonical_storage::RewriteStage::Activated {
                        Err(invalid("schema table late failure"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
            );
            assert_eq!(r.state.schema_table, table);
            assert_eq!(r.state.schema_ordinals, bindings);
            assert_eq!(r.history(1, 1), expected);
            assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::Poisoned)));
            drop(r);
            let mut r = Registry::open(d.path()).unwrap();
            assert_eq!(r.state.schema_table, table);
            assert_eq!(r.state.schema_ordinals, bindings);
            assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Pruned);
            assert_eq!(r.history(1, 1)[0].record.schema, sr2);
            let newest = SchemaRef { version: 3, ..sr1 };
            r.register_schema(schema(newest)).unwrap();
            r.update(
                NewRecord {
                    namespace: 1,
                    id: 1,
                    schema: newest,
                    values: vec![],
                },
                2,
            )
            .unwrap();
            r.compact(Retention::KeepLast(2)).unwrap();
            assert_eq!(
                r.history(1, 1)
                    .iter()
                    .map(|v| v.record.schema)
                    .collect::<Vec<_>>(),
                vec![sr2, newest]
            );
            r.state.validate_schema_table().unwrap();
        }
    }
    #[test]
    fn invalid_or_wrong_existing_ordinal_is_corruption_and_poison_for_all_read_modes() {
        for mode in [
            HistoryReadMode::ReferenceFull,
            HistoryReadMode::Selective,
            HistoryReadMode::Batch,
        ] {
            for wrong_existing in [false, true] {
                let d = tempfile::tempdir().unwrap();
                let (mut r, sr) = setup(d.path());
                let other = SchemaRef { id: 9, ..sr };
                r.register_schema(schema(other)).unwrap();
                r.create(1, 1, sr, &[]).unwrap();
                let bad = if wrong_existing {
                    r.state.schema_ordinals[&other]
                } else {
                    usize::MAX
                };
                r.state.history_meta.get_mut(&(1, 1)).unwrap()[0].schema_ordinal = bad;
                assert!(matches!(
                    r.lookup_many(
                        &[LookupRequest {
                            namespace: 1,
                            id: 1,
                            revision: Some(1)
                        }],
                        mode
                    ),
                    Err(Error::Corruption { .. })
                ));
                assert!(matches!(r.lookup_many(&[], mode), Err(Error::Poisoned)));
            }
        }
    }
    #[test]
    fn all_schema_bindings_are_prevalidated_before_any_member_or_frame_mutation() {
        let d = tempfile::tempdir().unwrap();
        let (mut r, sr) = setup(d.path());
        let count = r.state.frames.len();
        let table = r.state.schema_table.clone();
        let rec = |id, schema| Record {
            namespace: 1,
            id,
            schema,
            revision: 1,
            payload: vec![0, 0],
        };
        let event = Event::Transaction(Transaction {
            id: [1; 16],
            records: vec![rec(1, sr), rec(2, SchemaRef { id: 99, ..sr })],
        });
        let bytes = canonical_codec::encode(&(1u16, &event)).unwrap();
        let seq = r.sequence + 1;
        let source = Arc::new(Entry {
            sequence: seq,
            payload: bytes.clone(),
        });
        assert!(matches!(
            r.state.apply(event, seq, Some(10000), Some(source), &bytes),
            Err(Error::UnknownSchema)
        ));
        assert!(r.state.records.is_empty());
        assert!(r.state.history_meta.is_empty());
        assert_eq!(r.state.frames.len(), count);
        assert_eq!(r.state.schema_table, table);
    }
    #[test]
    fn public_record_wire_and_revision_fields_keep_complete_schema_reference() {
        let record = Record {
            namespace: 7,
            id: 9,
            schema: SchemaRef {
                namespace: 7,
                id: 3,
                version: 2,
            },
            revision: 3,
            payload: vec![42, 43, 44],
        };
        assert_eq!(
            canonical_codec::encode(&record).unwrap(),
            vec![7, 9, 7, 3, 2, 3, 3, 42, 43, 44]
        );
        let revision = Revision {
            record,
            deleted: true,
            commit_sequence: 11,
        };
        // Revision deliberately has no serde wire; its public fields and nested Record remain unchanged.
        assert_eq!(
            canonical_codec::encode(&revision.record).unwrap(),
            vec![7, 9, 7, 3, 2, 3, 3, 42, 43, 44]
        );
        assert_eq!(
            revision.record.schema,
            SchemaRef {
                namespace: 7,
                id: 3,
                version: 2
            }
        );
        assert!(revision.deleted);
        assert_eq!(revision.commit_sequence, 11);
        let decoded: Record = canonical_codec::decode(&[7, 9, 7, 3, 2, 3, 3, 42, 43, 44]).unwrap();
        assert_eq!(decoded, revision.record);
    }
}
