//! Opt-in heap telemetry for profiling executables; never installs a global allocator.
use serde::Serialize;
#[cfg(test)]
use std::alloc::{GlobalAlloc, Layout};
use std::{alloc::System, marker::PhantomData, rc::Rc};
#[repr(usize)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Component {
    Unattributed,
    Harness,
    StorePayload,
    StoreContainers,
    StoreTemporary,
    StoreSnapshot,
    RegistryDecode,
    RegistryDefinitions,
    RegistryCurrent,
    RegistryHistory,
    RegistryHistoryMetadata,
    RegistryRelations,
    RegistrySnapshots,
    RegistryReceipts,
    PrimaryReplay,
    PrimaryCache,
    QueryRows,
    QueryScalars,
    QueryIndexes,
    QueryCursors,
    QueryTemporary,
    GraphNodes,
    GraphEdges,
    GraphAdjacency,
}
pub const COMPONENT_COUNT: usize = 24;
pub const ALL_COMPONENTS: [Component; 24] = [
    Component::Unattributed,
    Component::Harness,
    Component::StorePayload,
    Component::StoreContainers,
    Component::StoreTemporary,
    Component::StoreSnapshot,
    Component::RegistryDecode,
    Component::RegistryDefinitions,
    Component::RegistryCurrent,
    Component::RegistryHistory,
    Component::RegistryHistoryMetadata,
    Component::RegistryRelations,
    Component::RegistrySnapshots,
    Component::RegistryReceipts,
    Component::PrimaryReplay,
    Component::PrimaryCache,
    Component::QueryRows,
    Component::QueryScalars,
    Component::QueryIndexes,
    Component::QueryCursors,
    Component::QueryTemporary,
    Component::GraphNodes,
    Component::GraphEdges,
    Component::GraphAdjacency,
];
impl Component {
    pub fn name(self) -> &'static str {
        const NAMES: [&str; 24] = [
            "unattributed",
            "harness",
            "store_payload",
            "store_containers",
            "store_temporary",
            "store_snapshot",
            "registry_decode",
            "registry_definitions",
            "registry_current",
            "registry_history",
            "registry_history_metadata",
            "registry_relations",
            "registry_snapshots",
            "registry_receipts",
            "primary_replay",
            "primary_cache",
            "query_rows",
            "query_scalars",
            "query_indexes",
            "query_cursors",
            "query_temporary",
            "graph_nodes",
            "graph_edges",
            "graph_adjacency",
        ];
        NAMES[self as usize]
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ComponentStats {
    pub current_requested: u64,
    pub peak_requested: u64,
    pub total_allocated: u64,
    pub total_freed: u64,
    pub live_blocks: u64,
    pub alloc_calls: u64,
    pub dealloc_calls: u64,
    pub realloc_calls: u64,
    pub current_overhead: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub components: [ComponentStats; 24],
    pub total_current_requested: u64,
    pub global_peak_requested: u64,
    /// Header/padding bytes at the same requested-heap peak event, not their own peak.
    pub global_peak_overhead: u64,
    pub peak_components: [u64; 24],
    pub event_sequence: u64,
    pub overflowed: bool,
    pub current_overhead: u64,
}
pub struct TrackingAllocator<A = System> {
    inner: A,
}
impl<A> TrackingAllocator<A> {
    pub const fn new(inner: A) -> Self {
        Self { inner }
    }
}
/// Thread-affine guard: a profiling scope cannot be moved to another thread.
/// ```compile_fail
/// let guard=canonical_memory::scope(canonical_memory::Component::QueryRows);
/// std::thread::spawn(move||drop(guard));
/// ```
/// Nor shared across threads:
/// ```compile_fail
/// fn require_sync<T:Sync>(){}
/// require_sync::<canonical_memory::ScopeGuard>();
/// ```
pub struct ScopeGuard {
    previous: usize,
    active: bool,
    _not_send: PhantomData<Rc<()>>,
}
mod tracker;
pub use tracker::{
    census_set, census_snapshot, clear_census, enabled, reset_peak_window, scope, snapshot,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ptr,
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };
    static TESTS: Mutex<()> = Mutex::new(());
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        TESTS.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn prime() {
        let a = TrackingAllocator::new(System);
        let l = Layout::from_size_align(1, 1).unwrap();
        let p = unsafe { a.alloc(l) };
        assert!(!p.is_null());
        unsafe { a.dealloc(p, l) };
    }
    fn consistent(s: Snapshot) {
        assert!(!s.overflowed);
        for c in s.components {
            assert_eq!(c.total_allocated - c.total_freed, c.current_requested);
            assert_eq!(c.alloc_calls - c.dealloc_calls, c.live_blocks);
            assert!(c.current_requested <= c.peak_requested);
            assert!(c.peak_requested <= s.global_peak_requested);
            if c.live_blocks == 0 {
                assert_eq!(c.current_overhead, 0);
            }
        }
        assert_eq!(
            s.components
                .iter()
                .map(|c| c.current_requested)
                .sum::<u64>(),
            s.total_current_requested
        );
        assert_eq!(
            s.peak_components.iter().sum::<u64>(),
            s.global_peak_requested
        );
        assert_eq!(
            s.components.iter().map(|c| c.current_overhead).sum::<u64>(),
            s.current_overhead
        );
    }
    #[test]
    fn overhead_is_captured_at_the_same_requested_peak_and_reset_event() {
        let _lock = lock();
        prime();
        reset_peak_window();
        let a = TrackingAllocator::new(System);
        let low = Layout::from_size_align(64, 8).unwrap();
        let high = Layout::from_size_align(32, 4096).unwrap();
        let p = unsafe { a.alloc(low) };
        assert!(!p.is_null());
        let q = unsafe { a.alloc(high) };
        assert!(!q.is_null());
        let at_peak = snapshot();
        assert_eq!(at_peak.global_peak_overhead, at_peak.current_overhead);
        unsafe { a.dealloc(q, high) };
        let later = snapshot();
        assert_eq!(later.global_peak_overhead, at_peak.global_peak_overhead);
        assert!(later.current_overhead < later.global_peak_overhead);
        reset_peak_window();
        let reset = snapshot();
        assert_eq!(reset.global_peak_overhead, reset.current_overhead);
        unsafe { a.dealloc(p, low) };
        consistent(snapshot());
    }
    #[test]
    fn component_ordinals_and_labels_are_unique_and_stable() {
        let mut names = std::collections::BTreeSet::new();
        assert_eq!(ALL_COMPONENTS.len(), COMPONENT_COUNT);
        for (ordinal, component) in ALL_COMPONENTS.iter().enumerate() {
            assert_eq!(*component as usize, ordinal);
            assert!(!component.name().is_empty());
            assert!(names.insert(component.name()));
        }
    }
    #[test]
    fn allocation_alignment_user_bytes_and_balanced_ledger() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(System);
        let before = snapshot();
        for align in [1, 2, 8, 16, 64, 4096] {
            for size in [1, 7, 65, 4097] {
                let l = Layout::from_size_align(size, align).unwrap();
                let p = {
                    let _s = scope(Component::StorePayload);
                    unsafe { a.alloc(l) }
                };
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0);
                unsafe { ptr::write_bytes(p, 0xA7, size) };
                assert!(
                    unsafe { std::slice::from_raw_parts(p, size) }
                        .iter()
                        .all(|b| *b == 0xA7)
                );
                let during = snapshot();
                consistent(during);
                assert_eq!(
                    during.components[Component::StorePayload as usize].current_requested
                        - before.components[Component::StorePayload as usize].current_requested,
                    size as u64
                );
                assert!(during.current_overhead > before.current_overhead);
                unsafe { a.dealloc(p, l) };
            }
        }
        let after = snapshot();
        consistent(after);
        assert_eq!(
            after.total_current_requested,
            before.total_current_requested
        );
        assert_eq!(after.current_overhead, before.current_overhead);
    }
    #[test]
    fn zeroed_allocation_preserves_zeroed_user_region() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(System);
        let l = Layout::from_size_align(1001, 256).unwrap();
        let p = {
            let _s = scope(Component::QueryRows);
            unsafe { a.alloc_zeroed(l) }
        };
        assert!(!p.is_null());
        assert_eq!(p as usize % 256, 0);
        assert!(
            unsafe { std::slice::from_raw_parts(p, 1001) }
                .iter()
                .all(|b| *b == 0)
        );
        unsafe { a.dealloc(p, l) };
        consistent(snapshot());
    }
    #[test]
    fn realloc_grow_shrink_preserves_bytes_alignment_origin_and_live_count() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(System);
        let before = snapshot();
        let l = Layout::from_size_align(13, 64).unwrap();
        let p = {
            let _s = scope(Component::RegistryHistory);
            unsafe { a.alloc(l) }
        };
        assert!(!p.is_null());
        unsafe { ptr::write_bytes(p, 0x5B, 13) };
        let p = {
            let _s = scope(Component::QueryTemporary);
            unsafe { a.realloc(p, l, 129) }
        };
        assert!(!p.is_null());
        assert_eq!(p as usize % 64, 0);
        assert!(
            unsafe { std::slice::from_raw_parts(p, 13) }
                .iter()
                .all(|b| *b == 0x5B)
        );
        let s = snapshot();
        consistent(s);
        assert_eq!(
            s.components[Component::RegistryHistory as usize].current_requested
                - before.components[Component::RegistryHistory as usize].current_requested,
            129
        );
        assert_eq!(
            s.components[Component::QueryTemporary as usize].current_requested,
            before.components[Component::QueryTemporary as usize].current_requested
        );
        let l2 = Layout::from_size_align(129, 64).unwrap();
        let p = unsafe { a.realloc(p, l2, 7) };
        assert!(!p.is_null());
        assert!(
            unsafe { std::slice::from_raw_parts(p, 7) }
                .iter()
                .all(|b| *b == 0x5B)
        );
        let l3 = Layout::from_size_align(7, 64).unwrap();
        unsafe { a.dealloc(p, l3) };
        let after = snapshot();
        consistent(after);
        assert_eq!(
            after.total_current_requested,
            before.total_current_requested
        );
        assert_eq!(
            after.components[Component::RegistryHistory as usize].realloc_calls
                - before.components[Component::RegistryHistory as usize].realloc_calls,
            2
        );
        assert_eq!(
            after.components[Component::RegistryHistory as usize].live_blocks,
            before.components[Component::RegistryHistory as usize].live_blocks
        );
    }
    struct Failure {
        fail_alloc: AtomicBool,
        fail_realloc: AtomicBool,
        alloc_attempts: AtomicUsize,
        realloc_attempts: AtomicUsize,
    }
    impl Failure {
        fn new() -> Self {
            Self {
                fail_alloc: AtomicBool::new(false),
                fail_realloc: AtomicBool::new(false),
                alloc_attempts: AtomicUsize::new(0),
                realloc_attempts: AtomicUsize::new(0),
            }
        }
    }
    // SAFETY: forwards unchanged System layouts; deliberate failure returns null without touching a block.
    unsafe impl GlobalAlloc for Failure {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            self.alloc_attempts.fetch_add(1, Ordering::Relaxed);
            if self.fail_alloc.load(Ordering::Relaxed) {
                ptr::null_mut()
            } else {
                unsafe { System.alloc(l) }
            }
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { System.dealloc(p, l) }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            self.realloc_attempts.fetch_add(1, Ordering::Relaxed);
            if self.fail_realloc.load(Ordering::Relaxed) {
                ptr::null_mut()
            } else {
                unsafe { System.realloc(p, l, n) }
            }
        }
    }
    #[test]
    fn failed_alloc_and_realloc_leave_original_block_and_ledger_unchanged() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(Failure::new());
        let l = Layout::from_size_align(37, 128).unwrap();
        a.inner.fail_alloc.store(true, Ordering::Relaxed);
        let before = snapshot();
        assert!(unsafe { a.alloc(l) }.is_null());
        assert_eq!(snapshot(), before);
        a.inner.fail_alloc.store(false, Ordering::Relaxed);
        let p = unsafe { a.alloc(l) };
        assert!(!p.is_null());
        unsafe { ptr::write_bytes(p, 0x73, 37) };
        a.inner.fail_realloc.store(true, Ordering::Relaxed);
        let before = snapshot();
        assert!(unsafe { a.realloc(p, l, 999) }.is_null());
        assert_eq!(snapshot(), before);
        assert!(
            unsafe { std::slice::from_raw_parts(p, 37) }
                .iter()
                .all(|b| *b == 0x73)
        );
        unsafe { a.dealloc(p, l) };
        consistent(snapshot());
    }
    #[test]
    fn header_layout_overflow_fails_before_system_without_real_oom() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(Failure::new());
        let huge = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
        let before = snapshot();
        assert!(unsafe { a.alloc(huge) }.is_null());
        assert_eq!(a.inner.alloc_attempts.load(Ordering::Relaxed), 0);
        assert_eq!(snapshot(), before);
        let l = Layout::from_size_align(19, 16).unwrap();
        let p = unsafe { a.alloc(l) };
        assert!(!p.is_null());
        let before = snapshot();
        assert!(unsafe { a.realloc(p, l, isize::MAX as usize) }.is_null());
        assert_eq!(a.inner.realloc_attempts.load(Ordering::Relaxed), 0);
        assert_eq!(snapshot(), before);
        unsafe { a.dealloc(p, l) };
    }
    #[test]
    fn nested_scope_restores_after_user_unwind_and_cross_thread_drop_uses_origin() {
        let _lock = lock();
        prime();
        let a = Arc::new(TrackingAllocator::new(System));
        let l = Layout::from_size_align(211, 8).unwrap();
        let before = snapshot();
        let p = {
            let _outer = scope(Component::StorePayload);
            let _ = std::panic::catch_unwind(|| {
                let _inner = scope(Component::QueryRows);
                panic!("user unwind outside allocator")
            });
            unsafe { a.alloc(l) }
        };
        assert!(!p.is_null());
        let s = snapshot();
        assert_eq!(
            s.components[Component::StorePayload as usize].current_requested
                - before.components[Component::StorePayload as usize].current_requested,
            211
        );
        let address = p as usize;
        let a2 = Arc::clone(&a);
        std::thread::spawn(move || {
            let _s = scope(Component::GraphEdges);
            unsafe { a2.dealloc(address as *mut u8, l) }
        })
        .join()
        .unwrap();
        let after = snapshot();
        assert_eq!(
            after.total_current_requested,
            before.total_current_requested
        );
        assert_eq!(
            after.components[Component::GraphEdges as usize].dealloc_calls,
            before.components[Component::GraphEdges as usize].dealloc_calls
        );
        consistent(after);
    }
    #[test]
    fn reset_window_preserves_live_counters_and_cumulative_calls() {
        let _lock = lock();
        prime();
        let a = TrackingAllocator::new(System);
        let l = Layout::from_size_align(57, 8).unwrap();
        let p = unsafe { a.alloc(l) };
        let q = unsafe { a.alloc(l) };
        assert!(!p.is_null() && !q.is_null());
        unsafe { a.dealloc(q, l) };
        let before = snapshot();
        reset_peak_window();
        let after = snapshot();
        consistent(after);
        assert_eq!(
            after.total_current_requested,
            before.total_current_requested
        );
        assert_eq!(after.global_peak_requested, after.total_current_requested);
        for i in 0..COMPONENT_COUNT {
            assert_eq!(
                after.components[i].current_requested,
                before.components[i].current_requested
            );
            assert_eq!(
                after.components[i].live_blocks,
                before.components[i].live_blocks
            );
            assert_eq!(
                after.components[i].alloc_calls,
                before.components[i].alloc_calls
            );
            assert_eq!(
                after.components[i].peak_requested,
                after.components[i].current_requested
            );
        }
        unsafe { a.dealloc(p, l) };
    }
    #[test]
    fn concurrent_events_and_peak_snapshots_are_reconciled_at_same_event() {
        let _lock = lock();
        prime();
        let a = Arc::new(TrackingAllocator::new(System));
        let barrier = Arc::new(Barrier::new(5));
        let before = snapshot();
        let mut workers = Vec::new();
        for component in [
            Component::StorePayload,
            Component::RegistryHistory,
            Component::QueryIndexes,
            Component::GraphAdjacency,
        ] {
            let a = Arc::clone(&a);
            let b = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let _s = scope(component);
                for i in 1..=250 {
                    let l = Layout::from_size_align(i + 13, 16).unwrap();
                    let p = unsafe { a.alloc(l) };
                    assert!(!p.is_null());
                    b.wait();
                    b.wait();
                    unsafe { a.dealloc(p, l) };
                }
            }));
        }
        for _ in 1..=250 {
            barrier.wait();
            consistent(snapshot());
            barrier.wait();
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let after = snapshot();
        consistent(after);
        assert_eq!(
            after.total_current_requested,
            before.total_current_requested
        );
        assert_eq!(after.current_overhead, before.current_overhead);
        assert!(after.event_sequence > before.event_sequence);
    }
    #[test]
    fn census_is_last_value_snapshot_is_owned_and_scope_restored() {
        let _lock = lock();
        prime();
        clear_census();
        census_set("history.payload_capacity_bytes", 100);
        census_set("history.payload_capacity_bytes", 7);
        census_set("query.rows", 3);
        let copy = census_snapshot();
        assert_eq!(copy.get("history.payload_capacity_bytes"), Some(&7));
        clear_census();
        assert!(census_snapshot().is_empty());
        assert_eq!(copy.get("query.rows"), Some(&3));
        let a = TrackingAllocator::new(System);
        let l = Layout::from_size_align(13, 1).unwrap();
        let before = snapshot();
        let p = unsafe { a.alloc(l) };
        assert!(!p.is_null());
        let after = snapshot();
        assert_eq!(
            after.components[Component::Unattributed as usize].current_requested
                - before.components[Component::Unattributed as usize].current_requested,
            13
        );
        unsafe { a.dealloc(p, l) };
    }
}
