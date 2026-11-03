#![cfg(feature = "memory-profile")]
use canonical_memory::{Component, TrackingAllocator};
use canonical_registry::*;
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator::new(std::alloc::System);
static PROFILE_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[test]
fn actual_tracking_allocator_observes_retained_metadata_and_one_replayed_index() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "profile-test".into(),
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
    r.create(1, 1, sr, &[Some(Value::Bytes(vec![9; 4096]))])
        .unwrap();
    r.flush_index().unwrap();
    drop(r);
    canonical_memory::clear_census();
    let r = Registry::open(dir.path()).unwrap();
    assert!(canonical_memory::enabled());
    let census = canonical_memory::census_snapshot();
    assert_eq!(census["registry.history_meta.payload_len_bytes"], 0);
    assert_eq!(census["registry.history_meta.payload_capacity_bytes"], 0);
    assert_eq!(census["registry.history.payload_capacity_bytes"], 0);
    assert_eq!(census["registry.history.logical_available_items"], 1);
    assert_eq!(census["registry.history.materialized_items"], 0);
    assert_eq!(census["registry.primary.validation.replay_entries"], 1);
    assert_eq!(census["registry.primary.validation.cached_entries"], 0);
    assert_eq!(
        census["registry.primary.validation.validated_cache_entries"],
        1
    );
    assert_eq!(census["registry.primary.validation.stream_attempted"], 1);
    assert_eq!(census["registry.primary.validation.stream_matched"], 1);
    assert_eq!(
        census["registry.primary.validation.process_replay_live_requested_bytes"], 0,
        "E18 canonical timeline must not retain a duplicate primary tree"
    );
    assert_eq!(
        census["registry.primary.validation.process_cache_live_requested_bytes"],
        0
    );
    let counts = canonical_memory::snapshot();
    assert!(counts.components[Component::RegistryHistoryMetadata as usize].current_requested > 0);
    assert_eq!(
        counts.components[Component::PrimaryCache as usize].current_requested,
        0
    );
    assert!(counts.components[Component::PrimaryCache as usize].peak_requested > 0);
    assert_eq!(census["registry.primary.canonical_metadata_source"], 1);
    assert_eq!(census["registry.primary.materialized_entries"], 0);
    assert_eq!(
        counts.components[Component::PrimaryReplay as usize].current_requested,
        0
    );
    let snap = r.snapshot();
    let clone = snap.clone();
    let census = canonical_memory::census_snapshot();
    assert_eq!(
        census["registry.snapshot.clone.last.arc_strong_refs"],
        if census.get("registry.snapshots.direct_map_access") == Some(&1) {
            4
        } else {
            2
        }
    );
    assert_eq!(
        snap.record(1, 1).unwrap().payload.as_ptr(),
        clone.record(1, 1).unwrap().payload.as_ptr()
    );
}

#[test]
fn streaming_cache_uses_real_bounded_buffers_instead_of_another_large_index_map() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "streaming-cache".into(),
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
            label: "n".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    let count = 10_000u64;
    for chunk in 0..10u64 {
        r.create_batch(
            (chunk + 1).to_le_bytes().repeat(2).try_into().unwrap(),
            &(1..=1000)
                .map(|i| NewRecord {
                    namespace: 1,
                    id: chunk * 1000 + i,
                    schema: sr,
                    values: vec![Some(Value::UInt(i))],
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    r.flush_index().unwrap();
    drop(r);
    canonical_memory::clear_census();
    let r = Registry::open_with_index_policy(d.path(), IndexPolicy::Full).unwrap();
    let counters = canonical_memory::snapshot();
    let c = canonical_memory::census_snapshot();
    assert!(r.index_status().loaded);
    assert_eq!(c["registry.primary.validation.replay_entries"], count);
    assert_eq!(c["registry.primary.validation.cached_entries"], 0);
    assert_eq!(
        c["registry.primary.validation.validated_cache_entries"],
        count
    );
    let cache = counters.components[Component::PrimaryCache as usize];
    assert_eq!(cache.current_requested, 0);
    assert!(cache.peak_requested > 0);
    assert!(
        cache.peak_requested < count * std::mem::size_of::<canonical_index::IndexEntry>() as u64
    );
    assert_eq!(
        counters.components[Component::PrimaryReplay as usize].current_requested,
        0
    );
}

#[test]
fn reopening_does_not_eagerly_materialize_public_history_payloads() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "lazy-history".into(),
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
    r.create(1, 1, sr, &[Some(Value::Bytes(vec![7; 4096]))])
        .unwrap();
    drop(r);
    canonical_memory::clear_census();
    let mut r = Registry::open(d.path()).unwrap();
    let c = canonical_memory::census_snapshot();
    assert_eq!(
        c["registry.history.payload_capacity_bytes"], 0,
        "cold public history must not own replay payloads"
    );
    assert_eq!(c["registry.history.lazy_public_cache"], 1);
    assert_eq!(c["registry.history.logical_available_items"], 1);
    assert_eq!(c["registry.history.materialized_items"], 0);
    assert_eq!(
        c["registry.history.prefix.shared_frames"],
        c["registry.history.prefix.frames"]
    );
    assert_eq!(c["registry.history.prefix.exclusive_payload_bytes"], 0);
    assert_eq!(c["registry.history.prefix.copied_payload_bytes"], 0);
    r.lookup(1, 1, Some(1)).unwrap();
    assert_eq!(
        canonical_memory::census_snapshot()["registry.history.materialized_items"],
        0
    );
    assert_eq!(r.history(1, 1).len(), 1);
    let first = r.history(1, 1).as_ptr();
    assert_eq!(r.history(1, 1).as_ptr(), first);
    let hot = canonical_memory::census_snapshot();
    assert_eq!(hot["registry.history.materialized_items"], 1);
    assert_eq!(hot["registry.history.materialized_identities"], 1);
    assert_eq!(
        hot["registry.history.element_capacity_bytes"],
        std::mem::size_of::<Revision>() as u64
    );
    assert_eq!(
        r.decode_values(sr, &r.history(1, 1)[0].record.payload)
            .unwrap(),
        vec![Some(Value::Bytes(vec![7; 4096]))]
    );
    r.update(
        NewRecord {
            namespace: 1,
            id: 1,
            schema: sr,
            values: vec![Some(Value::Bytes(vec![8; 17]))],
        },
        1,
    )
    .unwrap();
    let invalidated = canonical_memory::census_snapshot();
    assert_eq!(invalidated["registry.history.materialized_items"], 0);
    assert_eq!(invalidated["registry.history.materialized_identities"], 0);
    assert_eq!(invalidated["registry.history.payload_capacity_bytes"], 0);
    assert_eq!(invalidated["registry.history.logical_available_items"], 2);
    assert_eq!(r.history(1, 1).len(), 2);
    r.compact(Retention::KeepLast(1)).unwrap();
    let compacted = canonical_memory::census_snapshot();
    assert_eq!(compacted["registry.history.logical_available_items"], 1);
    assert_eq!(compacted["registry.history.materialized_items"], 0);
    assert_eq!(r.history(1, 1).len(), 1);
}

#[derive(Debug)]
struct AppendAllocationRow {
    revision: u64,
    realloc_calls: u64,
    alloc_calls: u64,
    requested_delta: u64,
    requested_before: u64,
    requested_after: u64,
}
fn public_metadata_append_measurements() -> (Option<u64>, u64, u64, Vec<AppendAllocationRow>) {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "allocation-growth".into(),
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
    for revision in 1..11 {
        r.update(input(), revision).unwrap();
    }
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    let census = canonical_memory::census_snapshot();
    let marker = census
        .get("registry.history_meta.replay_capacity_trim")
        .copied();
    let slots = census["registry.history_meta.capacity_slots"];
    assert_eq!(census["registry.history_meta.items"], 11);
    assert_eq!(
        census["registry.history_meta.element_capacity_bytes"],
        slots * 32
    );
    // Move the unrelated prefix descriptor buffer across its boundary before every measured version.
    for id in 2..=5 {
        r.register_namespace(Namespace {
            id,
            name: format!("spare-{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    let initial = canonical_memory::snapshot().components
        [Component::RegistryHistoryMetadata as usize]
        .current_requested;
    let mut rows = Vec::new();
    for revision in 11..=16 {
        let before =
            canonical_memory::snapshot().components[Component::RegistryHistoryMetadata as usize];
        let output = r.update(input(), revision).unwrap();
        let after =
            canonical_memory::snapshot().components[Component::RegistryHistoryMetadata as usize];
        rows.push(AppendAllocationRow {
            revision: revision + 1,
            realloc_calls: after.realloc_calls - before.realloc_calls,
            alloc_calls: after.alloc_calls - before.alloc_calls,
            requested_delta: after.current_requested - before.current_requested,
            requested_before: before.current_requested,
            requested_after: after.current_requested,
        });
        assert_eq!(output.revision, revision + 1);
        assert_eq!(output.schema, sr);
        assert_eq!(
            r.lookup(1, 1, Some(revision + 1))
                .unwrap()
                .revision
                .unwrap()
                .record,
            output
        );
    }
    assert_eq!(r.history(1, 1).len(), 17);
    (marker, slots, initial, rows)
}
#[test]
fn replayed_metadata_pays_reallocation_only_when_crossing_a_capacity_boundary() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (marker, slots, _, rows) = public_metadata_append_measurements();
    assert_eq!(marker, Some(1));
    assert_eq!(slots, 11);
    for row in rows {
        let (calls, bytes) = match row.revision {
            12 => (1, 5 * 32),
            17 => (1, 16 * 32),
            _ => (0, 0),
        };
        assert_eq!(row.realloc_calls, calls, "revision {}", row.revision);
        assert_eq!(row.requested_delta, bytes, "revision {}", row.revision);
        assert_eq!(
            row.alloc_calls, 0,
            "existing identity must not allocate metadata scratch per push"
        );
    }
}
#[test]
#[ignore = "finite same-file baseline/candidate Tracker diagnostic, invoked explicitly by root"]
fn public_append_allocation_counterfactual_helper() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let (marker, slots, initial, rows) = public_metadata_append_measurements();
    println!(
        "E23_APPEND_ALLOC phase=initial_after_source_buffer_warmup revision=11 trim_marker={marker:?} capacity_slots={slots} metadata_current_requested={initial}"
    );
    for row in rows {
        println!(
            "E23_APPEND_ALLOC phase=append revision={} realloc_calls={} alloc_calls={} requested_delta={} requested_before={} requested_after={}",
            row.revision,
            row.realloc_calls,
            row.alloc_calls,
            row.requested_delta,
            row.requested_before,
            row.requested_after
        );
    }
}

fn snapshot_cost_phase<T>(phase: &str, action: impl FnOnce() -> T) -> T {
    let before = canonical_memory::snapshot();
    let output = action();
    let after = canonical_memory::snapshot();
    let c = Component::RegistryCurrent as usize;
    let s = Component::RegistrySnapshots as usize;
    println!(
        "E24_SNAPSHOT_ALLOC phase={phase} current_alloc_calls={} current_realloc_calls={} current_requested_delta={} current_requested_after={} snapshots_alloc_calls={} snapshots_realloc_calls={} snapshots_requested_delta={} snapshots_requested_after={}",
        after.components[c].alloc_calls - before.components[c].alloc_calls,
        after.components[c].realloc_calls - before.components[c].realloc_calls,
        after.components[c].current_requested as i128
            - before.components[c].current_requested as i128,
        after.components[c].current_requested,
        after.components[s].alloc_calls - before.components[s].alloc_calls,
        after.components[s].realloc_calls - before.components[s].realloc_calls,
        after.components[s].current_requested as i128
            - before.components[s].current_requested as i128,
        after.components[s].current_requested
    );
    output
}
#[test]
#[ignore = "finite identical public-only E23/E24 Tracker allocation diagnostic invoked explicitly"]
fn public_snapshot_allocation_counterfactual_helper() {
    let _guard = PROFILE_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "cow-allocation".into(),
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
    let inputs = (1..=8)
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr,
            values: vec![Some(Value::Bytes(vec![id as u8; 4096]))],
        })
        .collect::<Vec<_>>();
    r.create_batch([1; 16], &inputs).unwrap();
    drop(r);
    let mut r = Registry::open(d.path()).unwrap();
    let input = |v| NewRecord {
        namespace: 1,
        id: 1,
        schema: sr,
        values: vec![Some(Value::Bytes(vec![v; 4096]))],
    };
    let census = canonical_memory::census_snapshot();
    println!(
        "E24_SNAPSHOT_ALLOC phase=initial shared_current_marker={:?} records={}",
        census.get("registry.current.shared_snapshot_map"),
        (1..=8).filter(|id| r.record(1, *id).is_some()).count()
    );
    snapshot_cost_phase("write_without_reader", || r.update(input(11), 1).unwrap());
    let expected = r.record(1, 1).unwrap().clone();
    let original = snapshot_cost_phase("snapshot_current", || r.snapshot());
    let clone = snapshot_cost_phase("clone_snapshot", || original.clone());
    let independent = snapshot_cost_phase("snapshot_current_again", || r.snapshot());
    snapshot_cost_phase("first_write_with_readers", || {
        r.update(input(12), 2).unwrap()
    });
    snapshot_cost_phase("second_write_detached", || r.update(input(13), 3).unwrap());
    assert_eq!(original.record(1, 1), Some(&expected));
    assert_eq!(clone.record(1, 1), Some(&expected));
    assert_eq!(independent.record(1, 1), Some(&expected));
    assert_eq!(r.record(1, 1).unwrap().revision, 4);
    drop(original);
    r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(r.revision_status(1, 1, 2), RevisionStatus::Available);
    drop(clone);
    r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(r.revision_status(1, 1, 2), RevisionStatus::Available);
    drop(independent);
    r.compact(Retention::KeepLast(1)).unwrap();
    assert_eq!(r.revision_status(1, 1, 2), RevisionStatus::Pruned);
    snapshot_cost_phase("write_after_drop_all", || r.update(input(14), 4).unwrap());
    assert_eq!(
        r.lookup(1, 1, None).unwrap().revision.unwrap().record,
        r.record(1, 1).unwrap().clone()
    );
}
