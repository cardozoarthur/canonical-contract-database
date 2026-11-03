//! Infallible public slices are reconstructed only from previously certified, retained bytes.
use super::*;
#[derive(Clone, Copy)]
pub(super) struct PayloadRange {
    pub start: u32,
    pub len: u32,
}
pub(super) struct FrameSource {
    pub offset: u64,
    pub entry: Option<Arc<Entry>>,
}
#[derive(Clone, Copy)]
pub(super) struct BorrowedRevision<'a> {
    pub record: history_read::RecordRef<'a>,
    pub deleted: bool,
    pub commit_sequence: u64,
}
impl State {
    pub(super) fn frame_source(&self, sequence: u64) -> Option<&FrameSource> {
        self.frames
            .get(usize::try_from(sequence.checked_sub(1)?).ok()?)
    }
    pub(super) fn expected_revision(
        &self,
        namespace: u64,
        id: u64,
        revision: u64,
    ) -> Option<BorrowedRevision<'_>> {
        if self.pruned.contains(&(namespace, id, revision)) {
            return None;
        }
        let slot = usize::try_from(revision.checked_sub(1)?).ok()?;
        let meta = self.history_meta.get(&(namespace, id))?.get(slot)?;
        let source = self.frame_source(meta.commit_sequence)?;
        let entry = source.entry.as_ref()?;
        if entry.sequence != meta.commit_sequence {
            return None;
        }
        let start = usize::try_from(meta.payload_start).ok()?;
        let end = start.checked_add(usize::try_from(meta.payload_len).ok()?)?;
        let payload = entry.payload.get(start..end)?;
        Some(BorrowedRevision {
            record: history_read::RecordRef {
                namespace,
                id,
                schema: self.metadata_schema(meta)?,
                revision,
                payload,
            },
            deleted: meta.deleted,
            commit_sequence: meta.commit_sequence,
        })
    }
    pub(super) fn public_history(&self, namespace: u64, id: u64) -> &[Revision] {
        let key = (namespace, id);
        let Some(cache) = self.history.get(&key) else {
            return &[];
        };
        cache
            .get_or_init(|| {
                let timeline = &self.history_meta[&key];
                let count = timeline
                    .iter()
                    .enumerate()
                    .filter(|(slot, _)| !self.pruned.contains(&(namespace, id, *slot as u64 + 1)))
                    .count();
                let revisions = profile_memory!(RegistryHistory, {
                    let mut revisions = Vec::with_capacity(count);
                    for slot in 0..timeline.len() {
                        let revision = slot as u64 + 1;
                        if self.pruned.contains(&(namespace, id, revision)) {
                            continue;
                        }
                        // Every range/source was proved before publication; no parse or disk operation here.
                        let expected = self
                            .expected_revision(namespace, id, revision)
                            .expect("certified immutable historical range");
                        revisions.push(Revision {
                            record: expected.record.owned(),
                            deleted: expected.deleted,
                            commit_sequence: expected.commit_sequence,
                        });
                    }
                    revisions.into_boxed_slice()
                });
                #[cfg(feature = "memory-profile")]
                memory::cache_initialized(self, &revisions);
                revisions
            })
            .as_ref()
    }
    pub(super) fn invalidate_public_history(&mut self, key: (u64, u64)) {
        let old = profile_memory!(RegistryHistory, self.history.entry(key).or_default().take());
        #[cfg(feature = "memory-profile")]
        if let Some(old) = &old {
            memory::cache_removed(self, old);
        }
        drop(old);
    }
    pub(super) fn validate_frame_source(
        &self,
        sequence: u64,
        offset: Option<u64>,
        entry: Option<&Arc<Entry>>,
        bytes: &[u8],
    ) -> Result<()> {
        if u64::try_from(self.frames.len())
            .map_err(|_| Error::TooLarge)?
            .checked_add(1)
            != Some(sequence)
        {
            return Err(invalid("noncontiguous prefix source"));
        }
        match (offset, entry) {
            (Some(offset), Some(entry))
                if offset >= 16
                    && entry.sequence == sequence
                    && (std::ptr::eq(entry.payload.as_slice(), bytes)
                        || entry.payload.as_slice() == bytes) =>
            {
                if self.frames.last().is_some_and(|p| p.offset >= offset) {
                    return Err(Error::Conflict);
                }
            }
            (None, None) => (),
            _ => return Err(invalid("invalid shared prefix source")),
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
            name: "lazy".into(),
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
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        (r, sr)
    }
    #[test]
    fn retained_prefix_supports_cold_and_hot_public_slices_after_late_rewrite_failure() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Registry>();
        for hot in [false, true] {
            for stage in [
                canonical_storage::RewriteStage::ManifestPublished,
                canonical_storage::RewriteStage::Activated,
            ] {
                let d = tempfile::tempdir().unwrap();
                let (mut r, sr) = setup(d.path());
                let first = r
                    .create(1, 1, sr, &[Some(Value::Bytes(vec![1; 4096]))])
                    .unwrap();
                let seq1 = r.sequence;
                let second = r
                    .update(
                        NewRecord {
                            namespace: 1,
                            id: 1,
                            schema: sr,
                            values: vec![Some(Value::Bytes(vec![2; 17]))],
                        },
                        1,
                    )
                    .unwrap();
                let expected = vec![
                    Revision {
                        record: first,
                        deleted: false,
                        commit_sequence: seq1,
                    },
                    Revision {
                        record: second,
                        deleted: false,
                        commit_sequence: r.sequence,
                    },
                ];
                if hot {
                    assert_eq!(r.history(1, 1), expected);
                } else {
                    assert!(r.state.history[&(1, 1)].get().is_none());
                }
                let old = r
                    .state
                    .frame_source(seq1)
                    .unwrap()
                    .entry
                    .as_ref()
                    .unwrap()
                    .clone();
                let old_capacity = r.state.history_meta[&(1, 1)].capacity();
                assert!(
                    r.compact_with_hook(Retention::KeepLast(1), |s| if s == stage {
                        Err(invalid("injected late rewrite failure"))
                    } else {
                        Ok(())
                    })
                    .is_err()
                );
                assert!(r.needs_reopen);
                assert_eq!(r.state.history_meta[&(1, 1)].capacity(), old_capacity);
                assert!(Arc::ptr_eq(
                    &old,
                    r.state.frame_source(seq1).unwrap().entry.as_ref().unwrap()
                ));
                assert_eq!(r.history(1, 1), expected);
                assert_eq!(r.revision(1, 1, 1), Some(&expected[0]));
                assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::Poisoned)));
            }
        }
    }
    #[test]
    fn cache_is_exact_repeatable_concurrent_and_invalidated_only_by_mutation() {
        let d = tempfile::tempdir().unwrap();
        let (mut r, sr) = setup(d.path());
        r.create(1, 1, sr, &[Some(Value::Bytes(vec![1; 31]))])
            .unwrap();
        assert!(r.state.history[&(1, 1)].get().is_none());
        for (index, source) in r.state.frames.iter().enumerate() {
            assert!(Arc::ptr_eq(
                source.entry.as_ref().unwrap(),
                &r.journal.prefix_entry(index as u64 + 1).unwrap()
            ));
        }
        assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Available);
        r.lookup(1, 1, Some(1)).unwrap();
        assert!(r.state.history[&(1, 1)].get().is_none());
        std::thread::scope(|scope| {
            let r = &r;
            let handles: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(move || {
                        let a = r.history(1, 1);
                        assert_eq!(a.len(), 1);
                        a.as_ptr() as usize
                    })
                })
                .collect();
            let addresses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert!(addresses.iter().all(|p| *p == addresses[0]));
        });
        assert_eq!(r.state.history[&(1, 1)].get().unwrap().len(), 1);
        r.update(
            NewRecord {
                namespace: 1,
                id: 1,
                schema: sr,
                values: vec![Some(Value::Bytes(vec![2; 7]))],
            },
            1,
        )
        .unwrap();
        assert!(r.state.history[&(1, 1)].get().is_none());
        assert_eq!(r.history(1, 1).len(), 2);
        r.compact(Retention::KeepLast(1)).unwrap();
        assert!(r.state.history[&(1, 1)].get().is_none());
        assert_eq!(
            r.history(1, 1)
                .iter()
                .map(|v| v.record.revision)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Pruned);
        assert_eq!(std::mem::size_of::<HistoryMeta>(), 32);
        assert_eq!(std::mem::size_of::<FrameSource>(), 16);
    }
}

#[cfg(test)]
mod prefix_negative_tests {
    use super::*;
    #[test]
    fn source_gaps_mismatched_bytes_and_owned_members_are_rejected_before_publication() {
        let mut state = State::default();
        let record = Record {
            namespace: 1,
            id: 1,
            schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 1,
            },
            revision: 1,
            payload: vec![7; 31],
        };
        state.schemas.insert(
            record.schema,
            Schema {
                reference: record.schema,
                fields: vec![],
            },
        );
        state.schema_table.push(record.schema);
        state.schema_ordinals.insert(record.schema, 0);
        let bytes = canonical_codec::encode(&(1u16, Event::Record(record.clone()))).unwrap();
        let source = Arc::new(Entry {
            sequence: 1,
            payload: bytes.clone(),
        });
        assert!(
            state
                .apply(
                    Event::Record(record.clone()),
                    2,
                    Some(16),
                    Some(source.clone()),
                    &bytes
                )
                .is_err()
        );
        let mut mismatched = record.clone();
        mismatched.payload[0] = 9;
        assert!(
            state
                .apply(
                    Event::Record(mismatched),
                    1,
                    Some(16),
                    Some(source.clone()),
                    &bytes
                )
                .is_err()
        );
        let mut bad_bytes = bytes.clone();
        bad_bytes[0] = 2;
        assert!(
            state
                .apply(
                    Event::Record(record.clone()),
                    1,
                    Some(16),
                    Some(source.clone()),
                    &bad_bytes
                )
                .is_err()
        );
        assert!(state.records.is_empty());
        assert!(state.history.is_empty());
        assert!(state.history_meta.is_empty());
        assert!(state.frames.is_empty());
        state
            .apply(Event::Record(record), 1, Some(16), Some(source), &bytes)
            .unwrap();
        state.validate_located_timeline().unwrap();
    }
    #[test]
    fn cold_public_history_survives_physical_crc_failure_without_reading_disk() {
        use std::io::{Read, Seek, SeekFrom, Write};
        let d = tempfile::tempdir().unwrap();
        let mut r = Registry::open(d.path()).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "prefix".into(),
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
            fields: vec![],
        })
        .unwrap();
        let expected = r.create(1, 1, sr, &[]).unwrap();
        assert!(r.state.history[&(1, 1)].get().is_none());
        let offset = r
            .state
            .canonical_location(1, 1, Some(1))
            .unwrap()
            .frame_offset;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(d.path().join("segments/00000001.cbr"))
            .unwrap();
        file.seek(SeekFrom::Start(
            offset + canonical_codec::FRAME_HEADER as u64,
        ))
        .unwrap();
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        byte[0] ^= 1;
        file.seek(SeekFrom::Start(
            offset + canonical_codec::FRAME_HEADER as u64,
        ))
        .unwrap();
        file.write_all(&byte).unwrap();
        file.sync_all().unwrap();
        assert!(matches!(
            r.lookup(1, 1, Some(1)),
            Err(Error::Corruption { .. })
        ));
        let io = r.journal.io_stats();
        assert_eq!(r.history(1, 1)[0].record, expected);
        assert_eq!(r.revision(1, 1, 1).unwrap().record, expected);
        assert_eq!(r.journal.io_stats(), io);
    }
}
