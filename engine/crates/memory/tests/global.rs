use canonical_memory::*;
use std::alloc::{GlobalAlloc, Layout, System};
#[global_allocator]
static ALLOCATOR: TrackingAllocator<System> = TrackingAllocator::new(System);
fn reconciled(s: Snapshot) {
    assert!(!s.overflowed);
    assert_eq!(
        s.components
            .iter()
            .map(|x| x.current_requested)
            .sum::<u64>(),
        s.total_current_requested
    );
    assert_eq!(
        s.peak_components.iter().sum::<u64>(),
        s.global_peak_requested
    );
    assert_eq!(
        s.components.iter().map(|x| x.current_overhead).sum::<u64>(),
        s.current_overhead
    );
}
#[test]
fn actual_global_tracker_and_census_coexist_without_recursive_metadata_allocations() {
    assert!(enabled());
    clear_census();
    let before = snapshot();
    let layout = Layout::from_size_align(513, 256).unwrap();
    let pointer = {
        let _s = scope(Component::QueryRows);
        unsafe { ALLOCATOR.alloc(layout) }
    };
    assert!(!pointer.is_null());
    assert_eq!(pointer as usize % 256, 0);
    unsafe { std::ptr::write_bytes(pointer, 0xD1, 513) };
    let during = snapshot();
    reconciled(during);
    assert_eq!(
        during.components[Component::QueryRows as usize].current_requested
            - before.components[Component::QueryRows as usize].current_requested,
        513
    );
    let copied = std::thread::spawn(|| {
        for i in 0..100 {
            census_set("profile.metadata.last", i);
        }
        census_snapshot()
    })
    .join()
    .unwrap();
    assert_eq!(copied.get("profile.metadata.last"), Some(&99));
    reconciled(snapshot());
    let address = pointer as usize;
    std::thread::spawn(move || {
        let _s = scope(Component::GraphEdges);
        unsafe { ALLOCATOR.dealloc(address as *mut u8, layout) }
    })
    .join()
    .unwrap();
    let after = snapshot();
    reconciled(after);
    assert_eq!(
        after.components[Component::QueryRows as usize].current_requested,
        before.components[Component::QueryRows as usize].current_requested
    );
    clear_census();
    drop(copied);
    assert!(census_snapshot().is_empty());
    reconciled(snapshot());
}
