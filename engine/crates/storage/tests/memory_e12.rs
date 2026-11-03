#![cfg(feature = "memory-profile")]
use canonical_memory::{Component, TrackingAllocator, census_snapshot, clear_census, snapshot};
use canonical_storage::Store;
#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator::new(std::alloc::System);
#[test]
fn reopened_store_census_matches_payload_capacity_and_arc_snapshots_share_payload() {
    let d = tempfile::tempdir().unwrap();
    let mut s = Store::open(d.path()).unwrap();
    s.append(b"observed-payload").unwrap();
    drop(s);
    clear_census();
    let s = Store::open(d.path()).unwrap();
    let c = census_snapshot();
    assert_eq!(c["store.payload_len_bytes"], 16);
    assert_eq!(c["store.payload_capacity_bytes"], 16);
    assert_eq!(c["store.entries_len"], 1);
    let before = snapshot();
    let a = s.snapshot();
    let b = s.snapshot();
    assert!(std::sync::Arc::ptr_eq(&a[0], &b[0]));
    let after = snapshot();
    assert_eq!(
        after.components[Component::StorePayload as usize].current_requested,
        before.components[Component::StorePayload as usize].current_requested
    );
    assert!(after.components[Component::StoreSnapshot as usize].current_requested >= 16);
    drop(a);
    drop(b);
    drop(s);
}
