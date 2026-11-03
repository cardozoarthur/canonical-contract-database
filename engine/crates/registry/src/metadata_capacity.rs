//! Capacity is tightened only at complete reconstruction boundaries, never per append.
use super::*;
fn growth_target(len: usize, capacity: usize) -> Result<Option<usize>> {
    let required = len.checked_add(1).ok_or(Error::TooLarge)?;
    let maximum = (isize::MAX as usize) / std::mem::size_of::<HistoryMeta>();
    if required > maximum {
        return Err(Error::TooLarge);
    }
    if required <= capacity {
        return Ok(None);
    }
    let target = required
        .checked_next_power_of_two()
        .unwrap_or(maximum)
        .max(4)
        .min(maximum);
    Ok(Some(target))
}
fn prepare_push(timeline: &mut Vec<HistoryMeta>) -> Result<()> {
    if let Some(target) = growth_target(timeline.len(), timeline.capacity())? {
        timeline
            .try_reserve_exact(target - timeline.len())
            .map_err(|error| {
                Error::Io(std::io::Error::new(std::io::ErrorKind::OutOfMemory, error))
            })?;
    }
    Ok(())
}
impl State {
    pub(super) fn trim_replayed_metadata(&mut self) {
        profile_memory!(RegistryHistoryMetadata, {
            for timeline in self.history_meta.values_mut() {
                timeline.shrink_to_fit();
            }
        });
    }
    /// All logical checks already passed; allocation failures cannot publish an empty new identity.
    pub(super) fn prepare_metadata_growth(&mut self, event: &Event) -> Result<()> {
        let mut pending = Vec::new();
        for (record, _) in event_members(event) {
            let key = (record.namespace, record.id);
            if let Some(timeline) = self.history_meta.get_mut(&key) {
                profile_memory!(RegistryHistoryMetadata, prepare_push(timeline))?;
            } else {
                let mut timeline = Vec::new();
                profile_memory!(RegistryHistoryMetadata, prepare_push(&mut timeline))?;
                profile_memory!(RegistryHistoryMetadata, pending.push((key, timeline)));
            }
        }
        // No recoverable operation follows insertion; all vectors were reserved successfully.
        for (key, timeline) in pending {
            profile_memory!(
                RegistryHistoryMetadata,
                self.history_meta.insert(key, timeline)
            );
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_limit_uses_the_vec_byte_limit_without_an_artificial_ordinal_limit() {
        let maximum = (isize::MAX as usize) / std::mem::size_of::<HistoryMeta>();
        assert!(matches!(
            growth_target(usize::MAX, usize::MAX),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            growth_target(maximum, maximum),
            Err(Error::TooLarge)
        ));
        assert_eq!(
            growth_target(maximum - 1, maximum - 1).unwrap(),
            Some(maximum)
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    fn real_reopened_timelines_grow_at_the_required_boundaries_and_rewrites_keep_pruned_slots() {
        for (length, next_capacity) in [
            (1, 4),
            (4, 8),
            (11, 16),
            (16, 32),
            (17, 32),
            (31, 32),
            (32, 64),
        ] {
            let d = tempfile::tempdir().unwrap();
            let mut r = Registry::open(d.path()).unwrap();
            r.register_namespace(Namespace {
                id: 1,
                name: "growth".into(),
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
            r.create(1, 1, sr, &[]).unwrap();
            let input = || NewRecord {
                namespace: 1,
                id: 1,
                schema: sr,
                values: vec![],
            };
            for revision in 1..length {
                r.update(input(), revision).unwrap();
            }
            let expected = r.history(1, 1).to_vec();
            drop(r);
            let mut r = Registry::open(d.path()).unwrap();
            assert_eq!(r.state.history_meta[&(1, 1)].capacity(), length as usize);
            assert_eq!(r.history(1, 1), expected);
            r.update(input(), length).unwrap();
            assert_eq!(r.state.history_meta[&(1, 1)].capacity(), next_capacity);
            if length == 11 {
                for expected_revision in 12..=16 {
                    r.update(input(), expected_revision).unwrap();
                    assert_eq!(
                        r.state.history_meta[&(1, 1)].capacity(),
                        if expected_revision < 16 { 16 } else { 32 }
                    );
                }
            }
            let slots = r.state.history_meta[&(1, 1)].len();
            r.compact(Retention::KeepLast(1)).unwrap();
            assert_eq!(r.state.history_meta[&(1, 1)].len(), slots);
            assert_eq!(r.state.history_meta[&(1, 1)].capacity(), slots);
            assert_eq!(r.history(1, 1).len(), 1);
            assert!(matches!(r.lookup(1, 1, Some(1)), Err(Error::HistoryPruned)));
            assert_eq!(
                r.lookup(1, 1, None)
                    .unwrap()
                    .revision
                    .unwrap()
                    .record
                    .revision,
                slots as u64
            );
        }
    }
}
