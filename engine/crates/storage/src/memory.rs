//! Last-construction logical census; do not add it to allocator totals.
use super::*;
use canonical_memory::{Component, census_set, enabled, scope};
pub(super) fn publish_store(store: &Store) {
    if !enabled() {
        return;
    }
    let _collector = scope(Component::Harness);
    census_set("store.entries_len", store.entries.len() as u64);
    census_set(
        "store.payload_len_bytes",
        store.entries.iter().map(|e| e.payload.len() as u64).sum(),
    );
    census_set(
        "store.payload_capacity_bytes",
        store
            .entries
            .iter()
            .map(|e| e.payload.capacity() as u64)
            .sum(),
    );
    census_set(
        "store.entry_handles_capacity_bytes",
        (store.entries.capacity() * std::mem::size_of::<Arc<Entry>>()) as u64,
    );
    census_set(
        "store.frame_offsets_capacity_bytes",
        (store.frame_offsets.capacity() * std::mem::size_of::<u64>()) as u64,
    );
    census_set(
        "store.entry_inline_size_bytes",
        std::mem::size_of::<Entry>() as u64,
    );
    // Arc control blocks/container nodes come from allocator observations, not an invented size formula.
}
