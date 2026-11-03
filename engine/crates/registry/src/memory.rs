//! Private opt-in logical census; allocator origin and ownership are separate views.
use super::*;
use canonical_memory::{Component, census_set, enabled, scope};
fn payloads<'a>(records: impl Iterator<Item = &'a Record>) -> (u64, u64) {
    records.fold((0, 0), |(len, cap), r| {
        (
            len + r.payload.len() as u64,
            cap + r.payload.capacity() as u64,
        )
    })
}
#[derive(Default)]
pub(super) struct HistoryCacheStats {
    pub logical_available_items: u64,
    materialized_items: u64,
    materialized_identities: u64,
    element_capacity_bytes: u64,
    payload_len_bytes: u64,
    payload_capacity_bytes: u64,
}
fn cache_sizes(revisions: &[Revision]) -> (u64, u64, u64, u64) {
    let (len, cap) = payloads(revisions.iter().map(|r| &r.record));
    (
        revisions.len() as u64,
        (std::mem::size_of_val(revisions)) as u64,
        len,
        cap,
    )
}
pub(super) fn cache_initialized(state: &State, revisions: &[Revision]) {
    let (items, bytes, len, cap) = cache_sizes(revisions);
    {
        let mut c = state
            .history_cache_stats
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        c.materialized_items += items;
        c.materialized_identities += 1;
        c.element_capacity_bytes += bytes;
        c.payload_len_bytes += len;
        c.payload_capacity_bytes += cap;
    }
    publish_history_cache(state);
}
pub(super) fn cache_removed(state: &State, revisions: &[Revision]) {
    let (items, bytes, len, cap) = cache_sizes(revisions);
    let mut c = state
        .history_cache_stats
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    c.materialized_items -= items;
    c.materialized_identities -= 1;
    c.element_capacity_bytes -= bytes;
    c.payload_len_bytes -= len;
    c.payload_capacity_bytes -= cap;
}
pub(super) fn publish_history_cache(state: &State) {
    if !enabled() {
        return;
    }
    let c = state
        .history_cache_stats
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (key, value) in [
        ("registry.history.lazy_public_cache", 1),
        (
            "registry.history.logical_available_items",
            c.logical_available_items,
        ),
        ("registry.history.materialized_items", c.materialized_items),
        (
            "registry.history.materialized_identities",
            c.materialized_identities,
        ),
        ("registry.history.items", c.materialized_items),
        ("registry.history.identities", state.history.len() as u64),
        (
            "registry.history.element_capacity_bytes",
            c.element_capacity_bytes,
        ),
        ("registry.history.payload_len_bytes", c.payload_len_bytes),
        (
            "registry.history.payload_capacity_bytes",
            c.payload_capacity_bytes,
        ),
    ] {
        census_set(key, value);
    }
}
pub(super) fn publish_schema_table(state: &State) {
    if !enabled() {
        return;
    }
    for (key, value) in [
        ("registry.history_meta.shared_schema_table", 1),
        ("registry.history_meta.replay_capacity_trim", 1),
        (
            "registry.schema_table.entries",
            state.schema_table.len() as u64,
        ),
        (
            "registry.schema_table.element_capacity_bytes",
            (state.schema_table.capacity() * std::mem::size_of::<SchemaRef>()) as u64,
        ),
        (
            "registry.schema_table.binding_entries",
            state.schema_ordinals.len() as u64,
        ),
        (
            "registry.schema_table.schema_ref_size",
            std::mem::size_of::<SchemaRef>() as u64,
        ),
        (
            "registry.schema_table.ordinal_size",
            std::mem::size_of::<usize>() as u64,
        ),
    ] {
        census_set(key, value);
    }
}
pub(super) fn inventory_registry(registry: &Registry) -> BTreeMap<&'static str, u64> {
    let _collector = scope(Component::Harness);
    let state = &registry.state;
    let (current_len, current_cap) = payloads(state.records.values());
    let (hi, hc, hl, hp) =
        state
            .history
            .values()
            .filter_map(OnceLock::get)
            .fold((0, 0, 0, 0), |(i, b, l, c), v| {
                let (ni, nb, nl, nc) = cache_sizes(v);
                (i + ni, b + nb, l + nl, c + nc)
            });
    let materialized_identities =
        state.history.values().filter(|v| v.get().is_some()).count() as u64;
    let logical_available = state
        .canonical_entry_count()
        .expect("validated available metadata");
    let mut shared_frames = 0u64;
    let mut shared_bytes = 0u64;
    let mut exclusive_bytes = 0u64;
    for source in &state.frames {
        if let Some(entry) = &source.entry {
            if Arc::strong_count(entry) > 1 {
                shared_frames += 1;
                shared_bytes += entry.payload.len() as u64;
            } else {
                exclusive_bytes += entry.payload.len() as u64;
            }
        }
    }
    let mi = state.history_meta.values().map(|v| v.len() as u64).sum();
    let mc = state
        .history_meta
        .values()
        .map(|v| (v.capacity() * std::mem::size_of::<HistoryMeta>()) as u64)
        .sum();
    // HistoryMeta has no payload buffer; these are type-level zeros, not len-only samples.
    let (ml, mp) = (0, 0);
    let mut defs_len = 0u64;
    let mut defs_cap = 0u64;
    let mut fields_cap = 0u64;
    let mut enum_cap = 0u64;
    for ns in state.namespaces.values() {
        defs_len += ns.name.len() as u64;
        defs_cap += ns.name.capacity() as u64;
    }
    for c in state.concepts.values() {
        defs_len += c.contract.len() as u64;
        defs_cap += c.contract.capacity() as u64;
        if let Some(label) = &c.label {
            defs_len += label.len() as u64;
            defs_cap += label.capacity() as u64;
        }
    }
    for s in state.schemas.values() {
        fields_cap += (s.fields.capacity() * std::mem::size_of::<Field>()) as u64;
        for f in &s.fields {
            defs_len += f.label.len() as u64;
            defs_cap += f.label.capacity() as u64;
            if let FieldType::Enum { variants } = &f.kind {
                enum_cap += (variants.capacity() * std::mem::size_of::<u32>()) as u64;
            }
        }
    }
    let pins = registry.snapshots.lock().unwrap_or_else(|e| e.into_inner());
    let mut active_maps = 0u64;
    let mut snapshot_records = 0u64;
    let mut snapshot_payload_len = 0u64;
    let mut snapshot_payload_cap = 0u64;
    let mut strong_refs = 0u64;
    let mut reader_leases = 0u64;
    let mut reader_instances = 0u64;
    let mut shared_current_maps = 0u64;
    let mut exclusive_retained_maps = 0u64;
    let mut shared_current_payload_cap = 0u64;
    let mut exclusive_retained_payload_cap = 0u64;
    let mut seen = BTreeSet::new();
    for weak in pins.iter() {
        if let Some(reader) = weak.upgrade() {
            let instances = Arc::strong_count(&reader) - 1; // exclude this census observer
            if instances == 0 {
                continue;
            }
            reader_leases += 1;
            reader_instances += instances as u64;
            let records = &reader.records;
            if !seen.insert(Arc::as_ptr(records) as usize) {
                continue;
            }
            active_maps += 1;
            strong_refs += Arc::strong_count(records) as u64;
            snapshot_records += records.len() as u64;
            let (len, cap) = payloads(records.values());
            snapshot_payload_len += len;
            snapshot_payload_cap += cap;
            if Arc::ptr_eq(records, &state.records) {
                shared_current_maps += 1;
                shared_current_payload_cap += cap;
            } else {
                exclusive_retained_maps += 1;
                exclusive_retained_payload_cap += cap;
            }
        }
    }
    let result = BTreeMap::from([
        ("registry.census.commit_sequence", registry.sequence),
        ("registry.current.records", state.records.len() as u64),
        ("registry.current.shared_snapshot_map", 1),
        ("registry.snapshots.reader_leases_independent", 1),
        ("registry.snapshots.direct_map_access", 1),
        ("registry.snapshots.reader_leases", reader_leases),
        ("registry.snapshots.reader_instances", reader_instances),
        (
            "registry.snapshots.shared_current_maps",
            shared_current_maps,
        ),
        (
            "registry.snapshots.exclusive_retained_maps",
            exclusive_retained_maps,
        ),
        ("registry.snapshots.current_owner_refs", 1),
        (
            "registry.snapshots.shared_current_payload_capacity_bytes",
            shared_current_payload_cap,
        ),
        (
            "registry.snapshots.exclusive_retained_payload_capacity_bytes",
            exclusive_retained_payload_cap,
        ),
        ("registry.current.payload_len_bytes", current_len),
        ("registry.current.payload_capacity_bytes", current_cap),
        ("registry.history.identities", state.history.len() as u64),
        ("registry.history.items", hi),
        ("registry.history.lazy_public_cache", 1),
        (
            "registry.history.logical_available_items",
            logical_available,
        ),
        ("registry.history.materialized_items", hi),
        (
            "registry.history.materialized_identities",
            materialized_identities,
        ),
        ("registry.history.prefix.frames", state.frames.len() as u64),
        ("registry.history.prefix.shared_frames", shared_frames),
        ("registry.history.prefix.shared_payload_bytes", shared_bytes),
        (
            "registry.history.prefix.exclusive_payload_bytes",
            exclusive_bytes,
        ),
        ("registry.history.prefix.copied_payload_bytes", 0),
        (
            "registry.history.prefix.descriptor_capacity_bytes",
            (state.frames.capacity() * std::mem::size_of::<FrameSource>()) as u64,
        ),
        ("registry.history.element_capacity_bytes", hc),
        ("registry.history.payload_len_bytes", hl),
        ("registry.history.payload_capacity_bytes", hp),
        (
            "registry.history_meta.identities",
            state.history_meta.len() as u64,
        ),
        ("registry.history_meta.items", mi),
        ("registry.history_meta.shared_schema_table", 1),
        ("registry.history_meta.replay_capacity_trim", 1),
        (
            "registry.history_meta.capacity_slots",
            state
                .history_meta
                .values()
                .map(|v| v.capacity() as u64)
                .sum(),
        ),
        (
            "registry.schema_table.entries",
            state.schema_table.len() as u64,
        ),
        (
            "registry.schema_table.element_capacity_bytes",
            (state.schema_table.capacity() * std::mem::size_of::<SchemaRef>()) as u64,
        ),
        (
            "registry.schema_table.binding_entries",
            state.schema_ordinals.len() as u64,
        ),
        (
            "registry.schema_table.schema_ref_size",
            std::mem::size_of::<SchemaRef>() as u64,
        ),
        (
            "registry.schema_table.ordinal_size",
            std::mem::size_of::<usize>() as u64,
        ),
        ("registry.history_meta.element_capacity_bytes", mc),
        ("registry.history_meta.payload_len_bytes", ml),
        ("registry.history_meta.payload_capacity_bytes", mp),
        ("registry.history_meta.zero_len_retained_payloads", 0),
        (
            "registry.inline.history_meta_size",
            std::mem::size_of::<HistoryMeta>() as u64,
        ),
        ("registry.pruned.items", state.pruned.len() as u64),
        (
            "registry.definitions.namespaces",
            state.namespaces.len() as u64,
        ),
        ("registry.definitions.concepts", state.concepts.len() as u64),
        ("registry.definitions.schemas", state.schemas.len() as u64),
        ("registry.definitions.byte_buffer_len_bytes", defs_len),
        ("registry.definitions.byte_buffer_capacity_bytes", defs_cap),
        (
            "registry.definitions.field_element_capacity_bytes",
            fields_cap,
        ),
        ("registry.definitions.enum_element_capacity_bytes", enum_cap),
        ("registry.receipts.create", state.transactions.len() as u64),
        (
            "registry.receipts.changes",
            state.change_transactions.len() as u64,
        ),
        (
            "registry.relations.rules",
            state.relation_rules.len() as u64,
        ),
        ("registry.relations.edges", state.native_edges.len() as u64),
        (
            "registry.relations.source_buckets",
            state.by_source.len() as u64,
        ),
        (
            "registry.relations.target_buckets",
            state.by_target.len() as u64,
        ),
        (
            "registry.relations.source_memberships",
            state.by_source.values().map(|v| v.len() as u64).sum(),
        ),
        (
            "registry.relations.target_memberships",
            state.by_target.values().map(|v| v.len() as u64).sum(),
        ),
        (
            "registry.primary.available_entries",
            registry
                .state
                .canonical_entry_count()
                .expect("validated canonical source"),
        ),
        ("registry.snapshots.tracker_slots", pins.len() as u64),
        (
            "registry.snapshots.tracker_element_capacity_bytes",
            (pins.capacity() * std::mem::size_of::<Weak<SnapshotLease>>()) as u64,
        ),
        ("registry.snapshots.active_unique_maps", active_maps),
        ("registry.snapshots.active_record_items", snapshot_records),
        ("registry.snapshots.active_map_strong_refs", strong_refs),
        (
            "registry.snapshots.active_payload_len_bytes",
            snapshot_payload_len,
        ),
        (
            "registry.snapshots.active_payload_capacity_bytes",
            snapshot_payload_cap,
        ),
        (
            "registry.inline.record_size",
            std::mem::size_of::<Record>() as u64,
        ),
        (
            "registry.inline.revision_size",
            std::mem::size_of::<Revision>() as u64,
        ),
    ]);
    drop(pins);
    result
}
pub(super) fn inventory_snapshot(snapshot: &Snapshot) -> BTreeMap<&'static str, u64> {
    let _collector = scope(Component::Harness);
    let (len, cap) = payloads(snapshot.records.values());
    BTreeMap::from([
        (
            "registry.snapshot.last.commit_sequence",
            snapshot.commit_sequence,
        ),
        (
            "registry.snapshot.last.records",
            snapshot.records.len() as u64,
        ),
        ("registry.snapshot.last.payload_len_bytes", len),
        ("registry.snapshot.last.payload_capacity_bytes", cap),
        (
            "registry.snapshot.last.arc_strong_refs",
            Arc::strong_count(&snapshot.records) as u64,
        ),
        (
            "registry.snapshot.last.arc_weak_refs",
            Arc::weak_count(&snapshot.records) as u64,
        ),
    ])
}
pub(super) fn publish_registry(registry: &Registry) {
    if enabled() {
        for (key, value) in inventory_registry(registry) {
            census_set(key, value);
        }
    }
}
pub(super) fn publish_snapshot(snapshot: &Snapshot) {
    if enabled() {
        for (key, value) in inventory_snapshot(snapshot) {
            census_set(key, value);
        }
    }
}
/// Authoritative replay remains owned; streaming cache validation uses bounded temporary buffers.
/// Counts distinguish compared entries from materialized cached maps; requested bytes are
/// process-wide allocation origins at this instant, not invented per-Registry estimates.
pub(super) fn begin_open() {
    if !enabled() {
        return;
    }
    for key in [
        "registry.primary.materialized_entries",
        "registry.primary.canonical_metadata_source",
        "registry.primary.validation.replay_entries",
        "registry.primary.validation.cached_entries",
        "registry.primary.validation.validated_cache_entries",
        "registry.primary.validation.stream_attempted",
        "registry.primary.validation.stream_matched",
        "registry.primary.validation.process_replay_live_requested_bytes",
        "registry.primary.validation.process_cache_live_requested_bytes",
    ] {
        census_set(key, 0);
    }
}
pub(super) fn publish_primary_validation(replay_entries: u64, attempted: bool, matched: bool) {
    if !enabled() {
        return;
    }
    let moment = canonical_memory::snapshot();
    census_set("registry.primary.validation.replay_entries", replay_entries);
    census_set("registry.primary.validation.cached_entries", 0);
    census_set("registry.primary.materialized_entries", 0);
    census_set("registry.primary.canonical_metadata_source", 1);
    census_set(
        "registry.primary.validation.validated_cache_entries",
        if matched { replay_entries } else { 0 },
    );
    census_set(
        "registry.primary.validation.stream_attempted",
        u64::from(attempted),
    );
    census_set(
        "registry.primary.validation.stream_matched",
        u64::from(matched),
    );
    census_set(
        "registry.primary.validation.process_replay_live_requested_bytes",
        moment.components[Component::PrimaryReplay as usize].current_requested,
    );
    census_set(
        "registry.primary.validation.process_cache_live_requested_bytes",
        moment.components[Component::PrimaryCache as usize].current_requested,
    );
}
/// Arc clone is O(1); record last-clone sharing without walking every payload again.
pub(super) fn publish_snapshot_clone(snapshot: &Snapshot) {
    if enabled() {
        census_set(
            "registry.snapshot.clone.last.records",
            snapshot.records.len() as u64,
        );
        census_set(
            "registry.snapshot.clone.last.arc_strong_refs",
            Arc::strong_count(&snapshot.records) as u64,
        );
        census_set(
            "registry.snapshot.clone.last.reader_instances",
            Arc::strong_count(&snapshot._reader) as u64,
        );
        census_set(
            "registry.snapshot.clone.last.arc_weak_refs",
            Arc::weak_count(&snapshot.records) as u64,
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Registry, SchemaRef) {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registry::open(dir.path()).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "memory-test".into(),
            level: Level::Local,
        })
        .unwrap();
        let schema = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: schema,
            fields: vec![Field {
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        r.create(1, 1, schema, &[Some(Value::Bytes(vec![7; 4096]))])
            .unwrap();
        (dir, r, schema)
    }
    #[test]
    fn census_distinguishes_payload_free_metadata_from_owned_history() {
        let (_dir, mut r, schema) = fixture();
        r.update(
            NewRecord {
                namespace: 1,
                id: 1,
                schema,
                values: vec![Some(Value::Bytes(vec![8; 13]))],
            },
            1,
        )
        .unwrap();
        let cold = inventory_registry(&r);
        assert_eq!(cold["registry.history.items"], 0);
        assert_eq!(cold["registry.history.logical_available_items"], 2);
        assert_eq!(cold["registry.history.materialized_items"], 0);
        assert_eq!(cold["registry.history.prefix.copied_payload_bytes"], 0);
        assert_eq!(r.history(1, 1).len(), 2);
        let c = inventory_registry(&r);
        assert_eq!(c["registry.history.items"], 2);
        assert_eq!(c["registry.history_meta.items"], 2);
        assert_eq!(c["registry.history_meta.shared_schema_table"], 1);
        assert_eq!(c["registry.schema_table.entries"], 1);
        assert_eq!(c["registry.schema_table.binding_entries"], 1);
        assert_eq!(c["registry.schema_table.schema_ref_size"], 24);
        assert_eq!(c["registry.schema_table.ordinal_size"], 8);
        assert_eq!(c["registry.inline.history_meta_size"], 32);
        assert_eq!(
            c["registry.history_meta.element_capacity_bytes"],
            c["registry.history_meta.capacity_slots"] * 32
        );
        assert_eq!(
            c["registry.inline.history_meta_size"],
            std::mem::size_of::<HistoryMeta>() as u64
        );
        assert_eq!(
            c["registry.history_meta.element_capacity_bytes"],
            r.state
                .history_meta
                .values()
                .map(|v| (v.capacity() * std::mem::size_of::<HistoryMeta>()) as u64)
                .sum::<u64>()
        );
        assert!(std::mem::size_of::<HistoryMeta>() < std::mem::size_of::<Revision>());
        assert_eq!(c["registry.history_meta.payload_len_bytes"], 0);
        assert_eq!(c["registry.history_meta.payload_capacity_bytes"], 0);
        assert_eq!(c["registry.history_meta.zero_len_retained_payloads"], 0);
        assert!(c["registry.history.payload_len_bytes"] > 4096);
        assert!(
            c["registry.history.element_capacity_bytes"]
                >= 2 * std::mem::size_of::<Revision>() as u64
        );
    }
    #[test]
    fn snapshot_clone_shares_one_map_and_payload_instead_of_deep_copying() {
        let (_dir, r, _) = fixture();
        let snap = r.snapshot();
        let clone = snap.clone();
        let before = inventory_snapshot(&snap);
        let other = inventory_snapshot(&clone);
        assert_eq!(before["registry.snapshot.last.arc_strong_refs"], 4);
        assert_eq!(before["registry.snapshot.last.records"], 1);
        assert_eq!(before, other);
        assert_eq!(
            snap.record(1, 1).unwrap().payload.as_ptr(),
            clone.record(1, 1).unwrap().payload.as_ptr()
        );
        assert_eq!(
            r.record(1, 1).unwrap().payload.as_ptr(),
            snap.record(1, 1).unwrap().payload.as_ptr()
        );
    }
    #[test]
    fn reader_census_deduplicates_shared_maps_and_excludes_canonical_owner_and_observer() {
        let (_d, r, _) = fixture();
        let first = r.snapshot();
        let clone = first.clone();
        let second = r.snapshot();
        let c = inventory_registry(&r);
        assert_eq!(c["registry.current.shared_snapshot_map"], 1);
        assert_eq!(c["registry.snapshots.reader_leases_independent"], 1);
        assert_eq!(c["registry.snapshots.reader_leases"], 2);
        assert_eq!(c["registry.snapshots.reader_instances"], 3);
        assert_eq!(c["registry.snapshots.active_unique_maps"], 1);
        assert_eq!(c["registry.snapshots.shared_current_maps"], 1);
        assert_eq!(c["registry.snapshots.exclusive_retained_maps"], 0);
        assert_eq!(
            c["registry.snapshots.active_payload_capacity_bytes"],
            c["registry.current.payload_capacity_bytes"]
        );
        drop(first);
        drop(clone);
        drop(second);
        let c = inventory_registry(&r);
        assert_eq!(c["registry.snapshots.reader_leases"], 0);
        assert_eq!(c["registry.snapshots.reader_instances"], 0);
        assert_eq!(c["registry.snapshots.active_unique_maps"], 0);
        assert_eq!(c["registry.snapshots.current_owner_refs"], 1);
    }
}
