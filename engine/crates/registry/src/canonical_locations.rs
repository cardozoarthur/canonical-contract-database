//! Borrowed primary source over canonical metadata; no resident or transient primary map.
use super::*;
type CurrentMembers<'a> = (
    (u64, u64),
    std::iter::Enumerate<std::slice::Iter<'a, HistoryMeta>>,
);
#[derive(Clone)]
pub(super) struct CanonicalEntries<'a> {
    identities: std::collections::btree_map::Iter<'a, (u64, u64), Vec<HistoryMeta>>,
    current: Option<CurrentMembers<'a>>,
    pruned: &'a BTreeSet<(u64, u64, u64)>,
    frames: &'a [FrameSource],
}
impl Iterator for CanonicalEntries<'_> {
    type Item = Result<IndexEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((key, members)) = &mut self.current
                && let Some((slot, meta)) = members.next()
            {
                let revision = match u64::try_from(slot).ok().and_then(|s| s.checked_add(1)) {
                    Some(v) => v,
                    None => return Some(Err(Error::TooLarge)),
                };
                if self.pruned.contains(&(key.0, key.1, revision)) {
                    continue;
                }
                let Some(location) = meta.location(self.frames) else {
                    return Some(Err(invalid("unlocated canonical source")));
                };
                return Some(Ok(IndexEntry {
                    key: IndexKey {
                        namespace: key.0,
                        id: key.1,
                        revision,
                    },
                    location,
                }));
            }
            let (key, timeline) = self.identities.next()?;
            self.current = Some((*key, timeline.iter().enumerate()));
        }
    }
}
impl HistoryMeta {
    fn location(&self, frames: &[FrameSource]) -> Option<Location> {
        let index = usize::try_from(self.commit_sequence.checked_sub(1)?).ok()?;
        let source = frames.get(index)?;
        Some(Location {
            commit_sequence: self.commit_sequence,
            frame_offset: source.offset,
            member: u32::from(self.member),
            deleted: self.deleted,
        })
    }
}
impl State {
    pub(super) fn canonical_entries(&self) -> CanonicalEntries<'_> {
        CanonicalEntries {
            identities: self.history_meta.iter(),
            current: None,
            pruned: &self.pruned,
            frames: &self.frames,
        }
    }
    pub(super) fn canonical_entry_count(&self) -> Result<u64> {
        let total = self.history_meta.values().try_fold(0u64, |n, v| {
            n.checked_add(u64::try_from(v.len()).map_err(|_| Error::TooLarge)?)
                .ok_or(Error::TooLarge)
        })?;
        total
            .checked_sub(u64::try_from(self.pruned.len()).map_err(|_| Error::TooLarge)?)
            .ok_or(invalid("pruned metadata exceeds timeline"))
    }
    #[cfg(test)]
    pub(super) fn canonical_location(
        &self,
        namespace: u64,
        id: u64,
        revision: Option<u64>,
    ) -> Option<Location> {
        let timeline = self.history_meta.get(&(namespace, id))?;
        let slot = match revision {
            Some(rev) => usize::try_from(rev.checked_sub(1)?).ok()?,
            None => timeline.len().checked_sub(1)?,
        };
        timeline.get(slot).and_then(|m| m.location(&self.frames))
    }
    pub(super) fn checked_canonical_location(
        &self,
        namespace: u64,
        id: u64,
        revision: Option<u64>,
    ) -> Result<Option<Location>> {
        let Some(timeline) = self.history_meta.get(&(namespace, id)) else {
            return Ok(None);
        };
        let slot = match revision {
            Some(rev) => match rev.checked_sub(1).and_then(|n| usize::try_from(n).ok()) {
                Some(n) => n,
                None => return Ok(None),
            },
            None => match timeline.len().checked_sub(1) {
                Some(n) => n,
                None => return Ok(None),
            },
        };
        let Some(meta) = timeline.get(slot) else {
            return Ok(None);
        };
        let location = meta
            .location(&self.frames)
            .ok_or(Error::Corruption { offset: 0 })?;
        if self
            .frame_source(meta.commit_sequence)
            .is_none_or(|f| f.entry.is_none())
            || location.frame_offset < 16
            || location.member >= 1024
            || self.metadata_schema(meta).is_none()
        {
            return Err(Error::Corruption {
                offset: location.frame_offset,
            });
        }
        Ok(Some(location))
    }
    /// All checks happen before any apply mutates public records/history.
    pub(super) fn validate_member_locations(
        &self,
        event: &Event,
        sequence: u64,
        offset: Option<u64>,
    ) -> Result<()> {
        for (member, (record, _)) in event_members(event).into_iter().enumerate() {
            if record.id == 0
                || record.revision == 0
                || sequence == 0
                || member >= MAX_TRANSACTION_RECORDS
                || offset.is_some_and(|v| v < 16)
            {
                return Err(invalid("invalid canonical primary location"));
            }
            let timeline = self.history_meta.get(&(record.namespace, record.id));
            let length = timeline.map_or(0, Vec::len);
            if u64::try_from(length)
                .map_err(|_| Error::TooLarge)?
                .checked_add(1)
                != Some(record.revision)
            {
                return Err(Error::Conflict);
            }
            if let Some(previous) = timeline.and_then(|v| v.last())
                && (sequence <= previous.commit_sequence
                    || offset.is_some_and(|o| {
                        previous
                            .location(&self.frames)
                            .is_none_or(|p| o <= p.frame_offset)
                    }))
            {
                return Err(Error::Conflict);
            }
            u16::try_from(member).map_err(|_| Error::TooLarge)?;
        }
        Ok(())
    }
    /// Rebind every private candidate descriptor to the newly rewritten Store prefix.
    pub(super) fn relocate_event(
        &mut self,
        event: &Event,
        sequence: u64,
        offset: u64,
        entry: Arc<Entry>,
    ) -> Result<()> {
        if offset < 16 || sequence == 0 || entry.sequence != sequence {
            return Err(invalid("invalid rewritten source"));
        }
        let source_index = usize::try_from(sequence - 1).map_err(|_| Error::TooLarge)?;
        if source_index > 0
            && self
                .frames
                .get(source_index - 1)
                .is_none_or(|p| p.entry.is_none() || p.offset >= offset)
        {
            return Err(Error::Conflict);
        }
        let ranges = history_read::payload_ranges(&entry.payload, event)?;
        let source = self.frames.get(source_index).ok_or(Error::NotFound)?;
        if source.entry.is_some() || source.offset != 0 {
            return Err(Error::Conflict);
        }
        for (ordinal, (record, deleted)) in event_members(event).into_iter().enumerate() {
            let slot = record
                .revision
                .checked_sub(1)
                .and_then(|s| usize::try_from(s).ok())
                .ok_or(Error::Conflict)?;
            let meta = self
                .history_meta
                .get(&(record.namespace, record.id))
                .and_then(|v| v.get(slot))
                .ok_or(Error::NotFound)?;
            let pruned = matches!(event,Event::Compacted(tx)if tx.members[ordinal].pruned);
            if self.metadata_schema(meta) != Some(record.schema)
                || meta.deleted != deleted
                || meta.commit_sequence != sequence
                || meta.member as usize != ordinal
                || self
                    .pruned
                    .contains(&(record.namespace, record.id, record.revision))
                    != pruned
            {
                return Err(invalid("rewritten canonical metadata mismatch"));
            }
            let meta = self
                .history_meta
                .get_mut(&(record.namespace, record.id))
                .and_then(|v| v.get_mut(slot))
                .ok_or(Error::NotFound)?;
            meta.payload_start = ranges[ordinal].start;
            meta.payload_len = ranges[ordinal].len;
        }
        self.frames[source_index] = FrameSource {
            offset,
            entry: Some(entry),
        };
        Ok(())
    }
    pub(super) fn validate_located_timeline(&self) -> Result<()> {
        self.validate_schema_table()?;
        let mut previous_offset = 0;
        for (index, source) in self.frames.iter().enumerate() {
            if source.offset < 16
                || source.offset <= previous_offset
                || source
                    .entry
                    .as_ref()
                    .is_none_or(|e| e.sequence != index as u64 + 1)
            {
                return Err(invalid("unlocated shared prefix"));
            }
            previous_offset = source.offset;
        }
        for ((namespace, id), timeline) in &self.history_meta {
            let mut previous: Option<Location> = None;
            for (slot, meta) in timeline.iter().enumerate() {
                let location = meta
                    .location(&self.frames)
                    .ok_or(invalid("missing canonical frame"))?;
                if *id == 0
                    || usize::from(meta.member) >= MAX_TRANSACTION_RECORDS
                    || self.metadata_schema(meta).is_none()
                {
                    return Err(invalid("unlocated canonical metadata"));
                }
                if previous.is_some_and(|p| {
                    p.frame_offset >= location.frame_offset
                        || p.commit_sequence >= location.commit_sequence
                }) {
                    return Err(Error::Conflict);
                }
                if !self.pruned.contains(&(*namespace, *id, slot as u64 + 1))
                    && self
                        .expected_revision(*namespace, *id, slot as u64 + 1)
                        .is_none()
                {
                    return Err(invalid("invalid certified historical range"));
                }
                previous = Some(location);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_state() -> State {
        let mut state = State::default();
        let sr = record(1, 1).schema;
        state.schemas.insert(
            sr,
            Schema {
                reference: sr,
                fields: vec![],
            },
        );
        state.schema_table.push(sr);
        state.schema_ordinals.insert(sr, 0);
        state
    }
    fn apply(state: &mut State, event: Event, sequence: u64, offset: Option<u64>) -> Result<()> {
        let bytes = canonical_codec::encode(&(1u16, &event))?;
        let entry = offset.map(|_| {
            Arc::new(Entry {
                sequence,
                payload: bytes.clone(),
            })
        });
        state.apply(event, sequence, offset, entry, &bytes)
    }
    fn relocate(state: &mut State, event: &Event, sequence: u64, offset: u64) -> Result<()> {
        let payload = canonical_codec::encode(&(1u16, event))?;
        state.relocate_event(
            event,
            sequence,
            offset,
            Arc::new(Entry { sequence, payload }),
        )
    }
    fn record(id: u64, revision: u64) -> Record {
        Record {
            namespace: 1,
            id,
            schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 1,
            },
            revision,
            payload: vec![],
        }
    }
    #[test]
    fn location_guards_prevalidate_all_members_before_any_public_mutation() {
        let mut state = fixture_state();
        let event = Event::Transaction(Transaction {
            id: [1; 16],
            records: vec![record(1, 1), record(2, 2)],
        });
        assert!(matches!(
            apply(&mut state, event, 1, Some(16)),
            Err(Error::Conflict)
        ));
        assert!(state.records.is_empty());
        assert!(state.history.is_empty());
        assert!(state.history_meta.is_empty());
        assert!(state.transactions.is_empty());
        assert!(apply(&mut state, Event::Record(record(1, 1)), 1, Some(0)).is_err());
        assert!(state.records.is_empty());
        apply(&mut state, Event::Record(record(1, 1)), 1, Some(16)).unwrap();
        for (seq, offset) in [(1, 100), (2, 16)] {
            assert!(apply(&mut state, Event::Record(record(1, 2)), seq, Some(offset)).is_err());
            assert_eq!(state.records[&(1, 1)].revision, 1);
            assert_eq!(state.history_meta[&(1, 1)].len(), 1);
        }
        assert!(state.canonical_location(1, 1, Some(0)).is_none());
        assert!(state.canonical_location(1, 1, Some(u64::MAX)).is_none());
    }
    #[test]
    fn placeholders_are_rejected_until_every_slot_is_relocated_and_metadata_must_match() {
        let mut state = fixture_state();
        apply(&mut state, Event::Record(record(1, 1)), 1, None).unwrap();
        apply(&mut state, Event::Record(record(1, 2)), 2, None).unwrap();
        assert!(state.validate_located_timeline().is_err());
        assert!(relocate(&mut state, &Event::Record(record(1, 2)), 2, 100).is_err());
        let mut bad = record(1, 1);
        bad.schema.version = 2;
        assert!(relocate(&mut state, &Event::Record(bad), 1, 16).is_err());
        relocate(&mut state, &Event::Record(record(1, 1)), 1, 16).unwrap();
        assert!(state.validate_located_timeline().is_err());
        assert!(relocate(&mut state, &Event::Record(record(1, u64::MAX)), 2, 100).is_err());
        relocate(&mut state, &Event::Record(record(1, 2)), 2, 100).unwrap();
        state.validate_located_timeline().unwrap();
        assert!(relocate(&mut state, &Event::Record(record(1, 1)), 1, 16).is_err());
    }
}
