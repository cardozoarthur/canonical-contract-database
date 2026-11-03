//! Historical member selection. Only called after Storage proves the entire disk payload
//! equals the immutable replay mirror; omitted member payloads remain borrowed.
use super::*;
use crate::projection::BorrowBytes;
use serde::de::{
    self, DeserializeSeed, Deserializer, EnumAccess, SeqAccess, VariantAccess, Visitor,
};
use std::fmt;

#[derive(Clone, Copy)]
pub(super) struct RecordRef<'de> {
    pub(super) namespace: u64,
    pub(super) id: u64,
    pub(super) schema: SchemaRef,
    pub(super) revision: u64,
    pub(super) payload: &'de [u8],
}
impl RecordRef<'_> {
    pub(super) fn owned(self) -> Record {
        Record {
            namespace: self.namespace,
            id: self.id,
            schema: self.schema,
            revision: self.revision,
            payload: self.payload.to_vec(),
        }
    }
    pub(super) fn matches(self, r: &Record) -> bool {
        self.namespace == r.namespace
            && self.id == r.id
            && self.schema == r.schema
            && self.revision == r.revision
            && self.payload == r.payload
    }
}
#[derive(Clone, Copy)]
struct Member<'de> {
    record: RecordRef<'de>,
    deleted: bool,
    pruned: bool,
}
struct Parts<'de> {
    visited: usize,
    selected: Vec<(usize, Member<'de>)>,
}
struct SelectedEnvelope<'de> {
    version: u16,
    parts: Parts<'de>,
}
struct EnvelopeSeed<'a> {
    selected: &'a [bool; MAX_TRANSACTION_RECORDS],
}
impl<'de> DeserializeSeed<'de> for EnvelopeSeed<'_> {
    type Value = SelectedEnvelope<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct("Envelope", &["version", "event"], self)
    }
}
impl<'de> Visitor<'de> for EnvelopeSeed<'_> {
    type Value = SelectedEnvelope<'de>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("canonical envelope")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let version = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("missing envelope version"))?;
        let parts = a
            .next_element_seed(EventSeed {
                selected: self.selected,
            })?
            .ok_or_else(|| de::Error::custom("missing event"))?;
        Ok(SelectedEnvelope { version, parts })
    }
}
struct RecordSeed {
    capture: bool,
}
impl<'de> DeserializeSeed<'de> for RecordSeed {
    type Value = Option<RecordRef<'de>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct(
            "Record",
            &["namespace", "id", "schema", "revision", "payload"],
            self,
        )
    }
}
impl<'de> Visitor<'de> for RecordSeed {
    type Value = Option<RecordRef<'de>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("record metadata and borrowed payload")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let namespace = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("record namespace"))?;
        let id = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("record id"))?;
        let schema = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("record schema"))?;
        let revision = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("record revision"))?;
        let payload = a
            .next_element_seed(BorrowBytes)?
            .ok_or_else(|| de::Error::custom("record payload"))?;
        Ok(self.capture.then_some(RecordRef {
            namespace,
            id,
            schema,
            revision,
            payload,
        }))
    }
}
struct ChangeSeed {
    capture: bool,
}
impl<'de> DeserializeSeed<'de> for ChangeSeed {
    type Value = Option<Member<'de>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct("Change", &["kind", "expected_revision", "record"], self)
    }
}
impl<'de> Visitor<'de> for ChangeSeed {
    type Value = Option<Member<'de>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("change")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let kind: ChangeKind = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("change kind"))?;
        let _: u64 = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("change expected revision"))?;
        let record = a
            .next_element_seed(RecordSeed {
                capture: self.capture,
            })?
            .ok_or_else(|| de::Error::custom("change record"))?;
        Ok(record.map(|record| Member {
            record,
            deleted: kind == ChangeKind::Delete,
            pruned: false,
        }))
    }
}
struct PrunedSeed {
    capture: bool,
}
impl<'de> DeserializeSeed<'de> for PrunedSeed {
    type Value = Option<Member<'de>>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct("PrunedMember", &["change", "pruned"], self)
    }
}
impl<'de> Visitor<'de> for PrunedSeed {
    type Value = Option<Member<'de>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("pruned member")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let member = a
            .next_element_seed(ChangeSeed {
                capture: self.capture,
            })?
            .ok_or_else(|| de::Error::custom("pruned change"))?;
        let pruned = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("pruned flag"))?;
        Ok(member.map(|m| Member { pruned, ..m }))
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Records,
    Changes,
    Pruned,
}
struct MembersSeed<'a> {
    selected: &'a [bool; MAX_TRANSACTION_RECORDS],
    kind: Kind,
}
impl<'de> DeserializeSeed<'de> for MembersSeed<'_> {
    type Value = Parts<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for MembersSeed<'_> {
    type Value = Parts<'de>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded members")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let count = a
            .size_hint()
            .ok_or_else(|| de::Error::custom("missing member count"))?;
        if count == 0 || count > MAX_TRANSACTION_RECORDS {
            return Err(de::Error::custom("member count limit"));
        }
        let mut selected =
            Vec::with_capacity(self.selected[..count].iter().filter(|x| **x).count());
        for ordinal in 0..count {
            let capture = self.selected[ordinal];
            let value = match self.kind {
                Kind::Records => a
                    .next_element_seed(RecordSeed { capture })?
                    .ok_or_else(|| de::Error::custom("member record"))?
                    .map(|record| Member {
                        record,
                        deleted: false,
                        pruned: false,
                    }),
                Kind::Changes => a
                    .next_element_seed(ChangeSeed { capture })?
                    .ok_or_else(|| de::Error::custom("member change"))?,
                Kind::Pruned => a
                    .next_element_seed(PrunedSeed { capture })?
                    .ok_or_else(|| de::Error::custom("member pruned"))?,
            };
            if let Some(value) = value {
                selected.push((ordinal, value));
            }
        }
        Ok(Parts {
            visited: count,
            selected,
        })
    }
}
struct TransactionSeed<'a> {
    selected: &'a [bool; MAX_TRANSACTION_RECORDS],
    kind: Kind,
}
impl<'de> DeserializeSeed<'de> for TransactionSeed<'_> {
    type Value = Parts<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct("Transaction", &["id", "records"], self)
    }
}
impl<'de> Visitor<'de> for TransactionSeed<'_> {
    type Value = Parts<'de>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("transaction members")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let _: TransactionId = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("transaction id"))?;
        a.next_element_seed(MembersSeed {
            selected: self.selected,
            kind: self.kind,
        })?
        .ok_or_else(|| de::Error::custom("transaction members"))
    }
}
struct CompactedSeed<'a> {
    selected: &'a [bool; MAX_TRANSACTION_RECORDS],
}
impl<'de> DeserializeSeed<'de> for CompactedSeed<'_> {
    type Value = Parts<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct(
            "PrunedEvent",
            &["version", "kind", "tx_id", "fingerprint", "members"],
            self,
        )
    }
}
impl<'de> Visitor<'de> for CompactedSeed<'_> {
    type Value = Parts<'de>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("compacted members")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Self::Value, A::Error> {
        let version: u16 = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("compacted version"))?;
        let kind: PrunedKind = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("compacted kind"))?;
        let tx: Option<TransactionId> = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("compacted tx"))?;
        let fingerprint: Option<[u8; 32]> = a
            .next_element()?
            .ok_or_else(|| de::Error::custom("compacted fingerprint"))?;
        if version != 1 {
            return Err(de::Error::custom("compacted version"));
        }
        let transactional = matches!(kind, PrunedKind::Transaction | PrunedKind::Changes);
        if transactional != tx.is_some() || transactional != fingerprint.is_some() {
            return Err(de::Error::custom("compacted receipt shape"));
        }
        let parts = a
            .next_element_seed(MembersSeed {
                selected: self.selected,
                kind: Kind::Pruned,
            })?
            .ok_or_else(|| de::Error::custom("compacted members"))?;
        if !transactional && parts.visited != 1 {
            return Err(de::Error::custom("compacted single cardinality"));
        }
        Ok(parts)
    }
}
struct EventSeed<'a> {
    selected: &'a [bool; MAX_TRANSACTION_RECORDS],
}
impl<'de> DeserializeSeed<'de> for EventSeed<'_> {
    type Value = Parts<'de>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_enum("Event", &[], self)
    }
}
impl<'de> Visitor<'de> for EventSeed<'_> {
    type Value = Parts<'de>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("record-bearing event")
    }
    fn visit_enum<A: EnumAccess<'de>>(self, a: A) -> std::result::Result<Self::Value, A::Error> {
        let (tag, v) = a.variant::<u32>()?;
        match tag {
            0 => {
                let _: Namespace = v.newtype_variant()?;
                Ok(Parts {
                    visited: 0,
                    selected: Vec::new(),
                })
            }
            1 => {
                let _: Concept = v.newtype_variant()?;
                Ok(Parts {
                    visited: 0,
                    selected: Vec::new(),
                })
            }
            2 => {
                let _: Schema = v.newtype_variant()?;
                Ok(Parts {
                    visited: 0,
                    selected: Vec::new(),
                })
            }
            7 => {
                let _: RelationRule = v.newtype_variant()?;
                Ok(Parts {
                    visited: 0,
                    selected: Vec::new(),
                })
            }
            3 => {
                let record = v.newtype_variant_seed(RecordSeed {
                    capture: self.selected[0],
                })?;
                Ok(Parts {
                    visited: 1,
                    selected: record
                        .map(|record| {
                            (
                                0,
                                Member {
                                    record,
                                    deleted: false,
                                    pruned: false,
                                },
                            )
                        })
                        .into_iter()
                        .collect(),
                })
            }
            4 => v.newtype_variant_seed(TransactionSeed {
                selected: self.selected,
                kind: Kind::Records,
            }),
            5 => v.newtype_variant_seed(TransactionSeed {
                selected: self.selected,
                kind: Kind::Changes,
            }),
            6 => {
                let member = v.newtype_variant_seed(ChangeSeed {
                    capture: self.selected[0],
                })?;
                Ok(Parts {
                    visited: 1,
                    selected: member.map(|m| (0, m)).into_iter().collect(),
                })
            }
            8 => v.newtype_variant_seed(CompactedSeed {
                selected: self.selected,
            }),
            _ => Err(de::Error::custom("event has no historical member")),
        }
    }
}
/// Ranges are certified while the full owned event is still validated and alive.
pub(super) fn payload_ranges(
    bytes: &[u8],
    event: &Event,
) -> Result<Vec<crate::lazy_history::PayloadRange>> {
    let expected = event_members(event);
    let selected = [true; MAX_TRANSACTION_RECORDS];
    let borrowed = canonical_codec::decode_seed(
        bytes,
        EnvelopeSeed {
            selected: &selected,
        },
    )?;
    if borrowed.version != 1
        || borrowed.parts.visited != expected.len()
        || borrowed.parts.selected.len() != expected.len()
    {
        return Err(invalid("prefix member shape mismatch"));
    }
    let mut ranges = Vec::with_capacity(expected.len());
    for (index, ((ordinal, member), (record, deleted))) in borrowed
        .parts
        .selected
        .into_iter()
        .zip(expected)
        .enumerate()
    {
        let pruned = matches!(event, Event::Compacted(tx) if tx.members[index].pruned);
        if ordinal != index
            || !member.record.matches(record)
            || member.deleted != deleted
            || member.pruned != pruned
        {
            return Err(invalid("prefix member metadata mismatch"));
        }
        let start = (member.record.payload.as_ptr() as usize)
            .checked_sub(bytes.as_ptr() as usize)
            .ok_or(invalid("payload is not borrowed from prefix"))?;
        let end = start
            .checked_add(member.record.payload.len())
            .ok_or(Error::TooLarge)?;
        if end > bytes.len() {
            return Err(invalid("payload range exceeds prefix"));
        }
        ranges.push(crate::lazy_history::PayloadRange {
            start: u32::try_from(start).map_err(|_| Error::TooLarge)?,
            len: u32::try_from(member.record.payload.len()).map_err(|_| Error::TooLarge)?,
        });
    }
    Ok(ranges)
}
#[derive(Clone, Copy)]
struct Plan {
    request: usize,
    location: Location,
    revision: u64,
}
impl Registry {
    pub(super) fn read_history_many(
        &mut self,
        requests: &[LookupRequest],
        mode: HistoryReadMode,
    ) -> Result<LookupBatchResult> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.ensure_healthy()?;
        if requests.len() > MAX_TRANSACTION_RECORDS {
            return Err(Error::TooLarge);
        }
        let mut plans = Vec::with_capacity(requests.len());
        let mut output_bytes = 0usize;
        for (i, request) in requests.iter().enumerate() {
            if request.revision.is_some_and(|rev| {
                self.state
                    .pruned
                    .contains(&(request.namespace, request.id, rev))
            }) {
                return Err(Error::HistoryPruned);
            }
            let location = match self.state.checked_canonical_location(
                request.namespace,
                request.id,
                request.revision,
            ) {
                Ok(Some(location)) => location,
                Ok(None) => continue,
                Err(error) => {
                    self.needs_reopen = true;
                    return Err(error);
                }
            };
            if request.revision.is_none() && location.deleted {
                continue;
            }
            let Some(revision) = request.revision.or_else(|| {
                self.record(request.namespace, request.id)
                    .map(|r| r.revision)
            }) else {
                self.needs_reopen = true;
                return Err(Error::Corruption {
                    offset: location.frame_offset,
                });
            };
            let Some(expected) =
                self.state
                    .expected_revision(request.namespace, request.id, revision)
            else {
                self.needs_reopen = true;
                return Err(Error::Corruption {
                    offset: location.frame_offset,
                });
            };
            output_bytes = output_bytes
                .checked_add(expected.record.payload.len())
                .ok_or(Error::TooLarge)?;
            if output_bytes > canonical_core::MAX_FRAME_BYTES {
                return Err(Error::TooLarge);
            }
            plans.push(Plan {
                request: i,
                location,
                revision,
            });
        }
        let mut result = LookupBatchResult {
            results: vec![
                LookupResult {
                    revision: None,
                    metrics: LookupMetrics {
                        index_probes: 1,
                        ..LookupMetrics::default()
                    }
                };
                requests.len()
            ],
            metrics: HistoryReadMetrics::default(),
        };
        let groups = if mode == HistoryReadMode::Batch {
            let mut map: BTreeMap<u64, Vec<Plan>> = BTreeMap::new();
            for p in plans {
                map.entry(p.location.frame_offset).or_default().push(p);
            }
            map.into_values().collect::<Vec<_>>()
        } else {
            plans.into_iter().map(|p| vec![p]).collect()
        };
        for group in groups {
            let offset = group[0].location.frame_offset;
            let before = self.journal.io_stats();
            let frame = match self.journal.read_verified_frame_at(offset) {
                Ok(f) => f,
                Err(e) => {
                    self.needs_reopen = true;
                    return Err(e);
                }
            };
            let after = self.journal.io_stats();
            let mut flags = [false; MAX_TRANSACTION_RECORDS];
            for p in &group {
                if p.location.member as usize >= MAX_TRANSACTION_RECORDS {
                    self.needs_reopen = true;
                    return Err(Error::Corruption { offset });
                }
                flags[p.location.member as usize] = true;
            }
            let full;
            let (version, parts) = if mode == HistoryReadMode::ReferenceFull {
                full = match canonical_codec::decode::<Envelope>(&frame.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        self.needs_reopen = true;
                        return Err(e);
                    }
                };
                let members = event_members(&full.event);
                let visited = members.len();
                let mut selected = Vec::new();
                for (ordinal, (record, deleted)) in members.into_iter().enumerate() {
                    if ordinal < flags.len() && flags[ordinal] {
                        let pruned =
                            matches!(&full.event,Event::Compacted(tx)if tx.members[ordinal].pruned);
                        selected.push((
                            ordinal,
                            Member {
                                record: RecordRef {
                                    namespace: record.namespace,
                                    id: record.id,
                                    schema: record.schema,
                                    revision: record.revision,
                                    payload: &record.payload,
                                },
                                deleted,
                                pruned,
                            },
                        ));
                    }
                }
                (full.version, Parts { visited, selected })
            } else {
                let selected = match canonical_codec::decode_seed(
                    &frame.payload,
                    EnvelopeSeed { selected: &flags },
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        self.needs_reopen = true;
                        return Err(e);
                    }
                };
                (selected.version, selected.parts)
            };
            if version != 1 {
                self.needs_reopen = true;
                return Err(Error::UnsupportedVersion(version));
            }
            let decoded = if mode == HistoryReadMode::ReferenceFull {
                parts.visited
            } else {
                parts.selected.len()
            };
            let first = group[0].request;
            result.results[first].metrics.point_reads = after.point_reads - before.point_reads;
            result.results[first].metrics.point_bytes = after.point_bytes - before.point_bytes;
            result.results[first].metrics.frames_scanned =
                after.replay_frames - before.replay_frames;
            result.results[first].metrics.decoded_members = decoded;
            result.metrics.physical_frame_reads += after.point_reads - before.point_reads;
            result.metrics.physical_bytes_read += after.point_bytes - before.point_bytes;
            // Unique CRC domains: header 20 bytes is hashed twice by the physical header/body path.
            result.metrics.header_crc_bytes_verified += 20;
            result.metrics.payload_crc_bytes_verified += frame.payload.len() as u64;
            result.metrics.prefix_payload_bytes_compared += frame.payload.len() as u64;
            result.metrics.members_visited += parts.visited as u64;
            result.metrics.members_decoded += decoded as u64;
            for p in group {
                let request = requests[p.request];
                let selected = parts
                    .selected
                    .iter()
                    .find(|(ordinal, _)| *ordinal == p.location.member as usize)
                    .map(|(_, m)| *m);
                let valid = selected
                    .filter(|m| {
                        !m.pruned
                            && m.record.namespace == request.namespace
                            && m.record.id == request.id
                            && m.record.revision == p.revision
                            && m.deleted == p.location.deleted
                            && frame.sequence == p.location.commit_sequence
                            && frame.sequence <= self.sequence
                    })
                    .filter(|m| {
                        self.state
                            .expected_revision(request.namespace, request.id, p.revision)
                            .is_some_and(|expected| {
                                m.record.namespace == expected.record.namespace
                                    && m.record.id == expected.record.id
                                    && m.record.schema == expected.record.schema
                                    && m.record.revision == expected.record.revision
                                    && m.record.payload == expected.record.payload
                                    && m.deleted == expected.deleted
                                    && expected.commit_sequence == frame.sequence
                            })
                    });
                let Some(member) = valid else {
                    self.needs_reopen = true;
                    return Err(Error::Corruption { offset });
                };
                result.metrics.output_records += 1;
                result.metrics.output_payload_bytes += member.record.payload.len() as u64;
                result.results[p.request].revision = Some(Revision {
                    record: member.record.owned(),
                    deleted: member.deleted,
                    commit_sequence: frame.sequence,
                });
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rec(id: u64) -> Record {
        Record {
            namespace: 0,
            id,
            schema: SchemaRef {
                namespace: 0,
                id: 1,
                version: 1,
            },
            revision: 1,
            payload: vec![id as u8; 7],
        }
    }
    fn parts<'de>(
        bytes: &'de [u8],
        selected: &[bool; MAX_TRANSACTION_RECORDS],
    ) -> Result<SelectedEnvelope<'de>> {
        canonical_codec::decode_seed(bytes, EnvelopeSeed { selected })
    }
    #[test]
    fn borrowed_seed_covers_every_envelope_variant_and_preserves_pruned_physical_slots() {
        let sr = rec(1).schema;
        let change = |id| Change {
            kind: ChangeKind::Create,
            expected_revision: 0,
            record: rec(id),
        };
        let events = vec![
            Event::Namespace(Namespace {
                id: 0,
                name: "global".into(),
                level: Level::Global,
            }),
            Event::Concept(Concept {
                namespace: 0,
                id: 1,
                version: 1,
                contract: vec![1],
                label: Some("c".into()),
            }),
            Event::Schema(Schema {
                reference: sr,
                fields: vec![],
            }),
            Event::Record(rec(1)),
            Event::Transaction(Transaction {
                id: [1; 16],
                records: vec![rec(1), rec(2)],
            }),
            Event::Changes(ChangeTransaction {
                id: [2; 16],
                changes: vec![change(1), change(2)],
            }),
            Event::Revision(change(1)),
            Event::RelationRule(RelationRule {
                schema: sr,
                kind: 1,
                source_schema: sr,
                target_schema: sr,
            }),
            Event::Compacted(PrunedEvent {
                version: 1,
                kind: PrunedKind::Changes,
                tx_id: Some([3; 16]),
                fingerprint: Some([4; 32]),
                members: vec![
                    PrunedMember {
                        change: Change {
                            record: Record {
                                payload: vec![],
                                ..rec(1)
                            },
                            ..change(1)
                        },
                        pruned: true,
                    },
                    PrunedMember {
                        change: change(2),
                        pruned: false,
                    },
                ],
            }),
        ];
        for event in events {
            let bytes = canonical_codec::encode(&Envelope { version: 1, event }).unwrap();
            let full: Envelope = canonical_codec::decode(&bytes).unwrap();
            let expected = event_members(&full.event);
            let mut selected = [false; MAX_TRANSACTION_RECORDS];
            selected[..expected.len()].fill(true);
            let out = parts(&bytes, &selected).unwrap();
            assert_eq!(out.version, 1);
            assert_eq!(out.parts.visited, expected.len());
            assert_eq!(out.parts.selected.len(), expected.len());
            for ((ordinal, member), (record, deleted)) in out.parts.selected.iter().zip(expected) {
                assert!(member.record.matches(record));
                let start = (member.record.payload.as_ptr() as usize)
                    .checked_sub(bytes.as_ptr() as usize)
                    .unwrap();
                let end = start.checked_add(member.record.payload.len()).unwrap();
                assert!(end <= bytes.len());
                assert_eq!(&bytes[start..end], record.payload.as_slice());
                let _: u32 = start.try_into().unwrap();
                let _: u32 = member.record.payload.len().try_into().unwrap();
                assert_eq!(member.deleted, deleted);
                assert_eq!(
                    member.pruned,
                    matches!(&full.event,Event::Compacted(tx)if tx.members[*ordinal].pruned)
                );
            }
        }
    }
    #[test]
    fn seed_rejects_truncation_trailing_unknown_tag_and_hostile_cardinality_without_member_vectors()
    {
        let bytes = canonical_codec::encode(&Envelope {
            version: 1,
            event: Event::Transaction(Transaction {
                id: [1; 16],
                records: vec![rec(1), rec(2)],
            }),
        })
        .unwrap();
        let mut selected = [false; MAX_TRANSACTION_RECORDS];
        selected[1] = true;
        for n in 0..bytes.len() {
            assert!(parts(&bytes[..n], &selected).is_err());
        }
        let mut bad = bytes.clone();
        bad.push(0);
        assert!(parts(&bad, &selected).is_err());
        let mut bad = bytes.clone();
        bad[1] = 99;
        assert!(parts(&bad, &selected).is_err());
        assert_eq!(bytes[18], 2);
        let mut bad = bytes[..18].to_vec();
        bad.extend_from_slice(&[129, 8]);
        bad.extend_from_slice(&bytes[19..]);
        assert!(parts(&bad, &selected).is_err());
        let mut unknown = bytes;
        unknown[0] = 2;
        assert_eq!(parts(&unknown, &selected).unwrap().version, 2);
    }
    #[test]
    fn internal_index_metadata_frame_mismatch_poison_is_not_empty_success() {
        for mode in [
            HistoryReadMode::ReferenceFull,
            HistoryReadMode::Selective,
            HistoryReadMode::Batch,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut r = Registry::open(dir.path()).unwrap();
            r.register_namespace(Namespace {
                id: 0,
                name: "global".into(),
                level: Level::Global,
            })
            .unwrap();
            let sr = rec(1).schema;
            r.register_schema(Schema {
                reference: sr,
                fields: vec![],
            })
            .unwrap();
            r.create(0, 1, sr, &[]).unwrap();
            let bad = &mut r.state.history_meta.get_mut(&(0, 1)).unwrap()[0];
            bad.commit_sequence = 1;
            bad.member = 0;
            assert!(matches!(
                r.lookup_many(
                    &[LookupRequest {
                        namespace: 0,
                        id: 1,
                        revision: Some(1)
                    }],
                    mode
                ),
                Err(Error::Corruption { offset: 16 })
            ));
            assert!(matches!(r.lookup_many(&[], mode), Err(Error::Poisoned)));
        }
    }
    #[test]
    fn missing_descriptor_for_an_existing_revision_is_corruption_not_missing() {
        for mode in [
            HistoryReadMode::ReferenceFull,
            HistoryReadMode::Selective,
            HistoryReadMode::Batch,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut r = Registry::open(dir.path()).unwrap();
            r.register_namespace(Namespace {
                id: 0,
                name: "source".into(),
                level: Level::Global,
            })
            .unwrap();
            let sr = rec(1).schema;
            r.register_schema(Schema {
                reference: sr,
                fields: vec![],
            })
            .unwrap();
            r.create(0, 1, sr, &[]).unwrap();
            r.state.history_meta.get_mut(&(0, 1)).unwrap()[0].commit_sequence = u64::MAX;
            assert_eq!(r.revision_status(0, 1, 1), RevisionStatus::Available);
            assert!(matches!(
                r.lookup_many(
                    &[LookupRequest {
                        namespace: 0,
                        id: 1,
                        revision: Some(1)
                    }],
                    mode
                ),
                Err(Error::Corruption { offset: 0 })
            ));
            assert!(matches!(r.lookup_many(&[], mode), Err(Error::Poisoned)));
        }
    }
}
