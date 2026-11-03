//! Retention preserves the commit timeline and identity metadata.
use super::*;
use canonical_storage::{CleanupReport, RewriteStage};

impl State {
    pub(super) fn validate_heads(&self) -> Result<()> {
        self.validate_schema_table()?;
        for ((ns, id), h) in &self.history_meta {
            if !h.is_empty() && self.pruned.contains(&(*ns, *id, h.len() as u64)) {
                return Err(invalid("compaction removed a current head"));
            }
        }
        Ok(())
    }
    pub(super) fn validate_pruned(&self, tx: &PrunedEvent) -> Result<()> {
        if tx.version != 1 {
            return Err(Error::UnsupportedVersion(tx.version));
        }
        if tx.members.is_empty() || tx.members.len() > MAX_TRANSACTION_RECORDS {
            return Err(invalid("compacted event cardinality"));
        }
        let transactional = matches!(tx.kind, PrunedKind::Transaction | PrunedKind::Changes);
        if transactional != tx.tx_id.is_some() || transactional != tx.fingerprint.is_some() {
            return Err(invalid("compacted receipt shape"));
        }
        if !transactional && tx.members.len() != 1 {
            return Err(invalid("compacted single event cardinality"));
        }
        if let Some(id) = tx.tx_id {
            if id == [0; 16] {
                return Err(invalid("zero transaction id"));
            }
            if self.transactions.contains_key(&id) || self.change_transactions.contains_key(&id) {
                return Err(Error::Conflict);
            }
        }
        let mut keys = BTreeSet::new();
        for member in &tx.members {
            let c = &member.change;
            let r = &c.record;
            if !keys.insert((r.namespace, r.id)) {
                return Err(Error::Conflict);
            }
            if matches!(tx.kind, PrunedKind::Record | PrunedKind::Transaction)
                && c.kind != ChangeKind::Create
            {
                return Err(invalid("compacted create kind"));
            }
            if member.pruned
                && (!r.payload.is_empty()
                    || c.kind == ChangeKind::Delete
                    || self.relation_rules.contains_key(&r.schema))
            {
                return Err(invalid("ineligible pruned member"));
            }
            if !self.schemas.contains_key(&r.schema) {
                return Err(Error::UnknownSchema);
            }
            self.validate_change_inner(c, member.pruned)?;
        }
        if tx.members.iter().all(|m| !m.pruned)
            && let Some(hash) = tx.fingerprint
        {
            let actual = match tx.kind {
                PrunedKind::Transaction => fingerprint(
                    &tx.members
                        .iter()
                        .map(|m| m.change.record.clone())
                        .collect::<Vec<_>>(),
                )?,
                PrunedKind::Changes => fingerprint(
                    &tx.members
                        .iter()
                        .map(|m| m.change.clone())
                        .collect::<Vec<_>>(),
                )?,
                _ => return Err(invalid("receipt on nontransaction")),
            };
            if hash != actual {
                return Err(invalid("compacted receipt fingerprint mismatch"));
            }
        }
        Ok(())
    }
    fn references(&self, schema: SchemaRef, payload: &[u8]) -> Result<Vec<Reference>> {
        let mut refs = Vec::new();
        self.collect_refs(schema, payload, &mut refs, 0)?;
        Ok(refs)
    }
    fn collect_refs(
        &self,
        sr: SchemaRef,
        payload: &[u8],
        out: &mut Vec<Reference>,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("reference nesting limit"));
        }
        let schema = self.schemas.get(&sr).ok_or(Error::UnknownSchema)?;
        for (field, value) in schema
            .fields
            .iter()
            .zip(self.decode_values(sr, payload, depth)?)
        {
            match value {
                Some(Value::Ref(r)) => out.push(r),
                Some(Value::Relation { source, target, .. }) => {
                    out.push(source);
                    out.push(target);
                }
                Some(Value::Array(items)) => {
                    for item in items {
                        if let ScalarValue::Ref(r) = item {
                            out.push(r);
                        }
                    }
                }
                Some(Value::Struct(bytes)) => {
                    if let FieldType::Struct(child) = field.kind {
                        self.collect_refs(child, &bytes, out, depth + 1)?;
                    }
                }
                _ => (),
            }
        }
        Ok(())
    }
}
impl Registry {
    /// KeepAll is the default policy. KeepLast is a lower bound: leases and references retain extras.
    pub fn compact(&mut self, policy: Retention) -> Result<CompactionReport> {
        self.compact_with_hook(policy, |_| Ok(()))
    }
    #[doc(hidden)]
    pub fn compact_with_hook<F: FnMut(RewriteStage) -> Result<()>>(
        &mut self,
        policy: Retention,
        hook: F,
    ) -> Result<CompactionReport> {
        self.ensure_healthy()?;
        if policy == Retention::KeepLast(0) {
            return Err(invalid("retention count must be positive"));
        }
        let mut keep = BTreeSet::new();
        for (key, timeline) in &self.state.history_meta {
            let available = timeline.iter().enumerate().filter(|(slot, _)| {
                !self
                    .state
                    .pruned
                    .contains(&(key.0, key.1, *slot as u64 + 1))
            });
            let count = match policy {
                Retention::KeepAll => timeline.len(),
                Retention::KeepLast(n) => n,
            };
            for (slot, _) in available.clone().rev().take(count) {
                keep.insert((key.0, key.1, slot as u64 + 1));
            }
            for (slot, meta) in available {
                if meta.deleted
                    || self
                        .state
                        .metadata_schema(meta)
                        .is_some_and(|schema| self.state.relation_rules.contains_key(&schema))
                {
                    keep.insert((key.0, key.1, slot as u64 + 1));
                }
            }
        }
        {
            let mut pins = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
            pins.retain(|pin| pin.strong_count() > 0);
            for snapshot in pins.iter().filter_map(Weak::upgrade) {
                for r in snapshot.records.values() {
                    keep.insert((r.namespace, r.id, r.revision));
                }
            }
        }
        let entries = self.journal.snapshot();
        let payloads = loop {
            self.protect_references(&mut keep)?;
            let mut payloads = Vec::with_capacity(entries.len());
            let mut expanded = false;
            for entry in &entries {
                let envelope: Envelope = canonical_codec::decode(&entry.payload)?;
                let Some(mut compacted) = to_pruned(envelope.event)? else {
                    payloads.push(entry.payload.clone());
                    continue;
                };
                for member in &mut compacted.members {
                    let r = &mut member.change.record;
                    if !keep.contains(&(r.namespace, r.id, r.revision)) {
                        member.pruned = true;
                        r.payload.clear();
                    }
                }
                if compacted.members.iter().all(|m| !m.pruned) {
                    payloads.push(entry.payload.clone());
                    continue;
                }
                match canonical_codec::encode(&Envelope {
                    version: 1,
                    event: Event::Compacted(compacted),
                }) {
                    Ok(payload) => payloads.push(payload),
                    Err(Error::TooLarge) => {
                        // Preserve the original atomic frame if wrapper overhead would exceed its limit.
                        let envelope: Envelope = canonical_codec::decode(&entry.payload)?;
                        for (r, _) in event_members(&envelope.event) {
                            if !self.state.pruned.contains(&(r.namespace, r.id, r.revision)) {
                                expanded |= keep.insert((r.namespace, r.id, r.revision));
                            }
                        }
                        payloads.push(entry.payload.clone());
                    }
                    Err(e) => return Err(e),
                }
            }
            if !expanded {
                break payloads;
            }
        };
        // Validate every candidate before touching durable files.
        let mut candidate = State::default();
        for (entry, payload) in entries.iter().zip(&payloads) {
            let e: Envelope = canonical_codec::decode(payload)?;
            candidate.validate(&e.event)?;
            candidate.apply(e.event, entry.sequence, None, None, payload)?;
        }
        candidate.validate_heads()?;
        candidate.trim_replayed_metadata();
        let already_pruned_revisions = self.state.pruned.len();
        let pruned_revisions = candidate
            .pruned
            .len()
            .checked_sub(already_pruned_revisions)
            .ok_or(Error::Conflict)?;
        let retained_revisions =
            usize::try_from(candidate.canonical_entry_count()?).map_err(|_| Error::TooLarge)?;
        let generation = match self.journal.rewrite_with_hook(&payloads, hook) {
            Ok(report) => report,
            Err(error) => {
                self.needs_reopen = true;
                drop(entries);
                #[cfg(feature = "memory-profile")]
                memory::publish_registry(self);
                return Err(error);
            }
        };
        drop(entries);
        let relocated = (|| -> Result<()> {
            for entry in self.journal.snapshot() {
                let e: Envelope = canonical_codec::decode(&entry.payload)?;
                if e.version != 1 {
                    return Err(Error::UnsupportedVersion(e.version));
                }
                let offset = self
                    .journal
                    .frame_offset(entry.sequence)
                    .ok_or(Error::Corruption { offset: 0 })?;
                candidate.relocate_event(&e.event, entry.sequence, offset, Arc::clone(&entry))?;
            }
            candidate.validate_located_timeline()
        })();
        if let Err(error) = relocated {
            self.needs_reopen = true;
            #[cfg(feature = "memory-profile")]
            memory::publish_registry(self);
            return Err(error);
        }
        self.state = candidate;
        self.index_status = IndexStatus {
            loaded: false,
            rebuilt: true,
            reason: Some(IndexRebuildReason::InvalidOrStale),
        };
        #[cfg(feature = "memory-profile")]
        memory::publish_registry(self);
        Ok(CompactionReport {
            generation,
            retained_revisions,
            pruned_revisions,
            already_pruned_revisions,
        })
    }
    fn protect_references(&self, keep: &mut BTreeSet<(u64, u64, u64)>) -> Result<()> {
        let mut pending: Vec<_> = keep.iter().copied().collect();
        let mut visited = BTreeSet::new();
        while let Some(key) = pending.pop() {
            if !visited.insert(key) {
                continue;
            }
            let Some(v) = self.state.expected_revision(key.0, key.1, key.2) else {
                return Err(Error::HistoryPruned);
            };
            if v.deleted {
                continue;
            }
            for reference in self.state.references(v.record.schema, v.record.payload)? {
                let Some((slot, _target)) = self
                    .state
                    .history_meta
                    .get(&(reference.namespace, reference.id))
                    .and_then(|h| {
                        h.iter()
                            .enumerate()
                            .rev()
                            .find(|(_, m)| m.commit_sequence <= v.commit_sequence)
                    })
                else {
                    continue;
                };
                let key = (
                    reference.namespace,
                    reference.id,
                    u64::try_from(slot)
                        .map_err(|_| Error::TooLarge)?
                        .checked_add(1)
                        .ok_or(Error::TooLarge)?,
                );
                if self.state.pruned.contains(&key) {
                    return Err(Error::HistoryPruned);
                }
                if keep.insert(key) {
                    pending.push(key);
                }
            }
        }
        Ok(())
    }
    /// Explicitly remove old physical generations only after a verified active generation.
    pub fn cleanup_obsolete_generations(&mut self) -> Result<CleanupReport> {
        self.ensure_healthy()?;
        self.journal.cleanup_obsolete_generations()
    }
}
fn to_pruned(event: Event) -> Result<Option<PrunedEvent>> {
    let create = |record| PrunedMember {
        change: Change {
            kind: ChangeKind::Create,
            expected_revision: 0,
            record,
        },
        pruned: false,
    };
    let change = |change| PrunedMember {
        change,
        pruned: false,
    };
    let (kind, tx_id, hash, members) = match event {
        Event::Record(r) => (PrunedKind::Record, None, None, vec![create(r)]),
        Event::Transaction(tx) => {
            let hash = fingerprint(&tx.records)?;
            (
                PrunedKind::Transaction,
                Some(tx.id),
                Some(hash),
                tx.records.into_iter().map(create).collect(),
            )
        }
        Event::Changes(tx) => {
            let hash = fingerprint(&tx.changes)?;
            (
                PrunedKind::Changes,
                Some(tx.id),
                Some(hash),
                tx.changes.into_iter().map(change).collect(),
            )
        }
        Event::Revision(c) => (PrunedKind::Revision, None, None, vec![change(c)]),
        Event::Compacted(tx) => return Ok(Some(tx)),
        _ => return Ok(None),
    };
    Ok(Some(PrunedEvent {
        version: 1,
        kind,
        tx_id,
        fingerprint: hash,
        members,
    }))
}
