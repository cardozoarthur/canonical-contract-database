use super::{COMPONENT_COUNT, Component, ComponentStats, ScopeGuard, Snapshot, TrackingAllocator};
use std::{
    alloc::{GlobalAlloc, Layout},
    cell::{Cell, UnsafeCell},
    collections::BTreeMap,
    marker::PhantomData,
    ptr,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
const EMPTY: ComponentStats = ComponentStats {
    current_requested: 0,
    peak_requested: 0,
    total_allocated: 0,
    total_freed: 0,
    live_blocks: 0,
    alloc_calls: 0,
    dealloc_calls: 0,
    realloc_calls: 0,
    current_overhead: 0,
};
const EMPTY_SNAPSHOT: Snapshot = Snapshot {
    components: [EMPTY; COMPONENT_COUNT],
    total_current_requested: 0,
    global_peak_requested: 0,
    global_peak_overhead: 0,
    peak_components: [0; COMPONENT_COUNT],
    event_sequence: 0,
    overflowed: false,
    current_overhead: 0,
};
fn add(x: &mut u64, n: u64, bad: &mut bool) {
    match x.checked_add(n) {
        Some(v) => *x = v,
        None => {
            *bad = true;
            *x = u64::MAX;
        }
    }
}
fn sub(x: &mut u64, n: u64, bad: &mut bool) {
    match x.checked_sub(n) {
        Some(v) => *x = v,
        None => {
            *bad = true;
            *x = 0;
        }
    }
}
struct Ledger {
    data: Snapshot,
}
enum Event {
    Alloc { size: u64, overhead: u64 },
    Free { size: u64, overhead: u64 },
    Realloc { old: u64, new: u64 },
}
impl Ledger {
    const fn new() -> Self {
        Self {
            data: EMPTY_SNAPSHOT,
        }
    }
    fn event(&mut self, origin: usize, event: Event) {
        let Some(c) = self.data.components.get_mut(origin) else {
            self.data.overflowed = true;
            return;
        };
        let bad = &mut self.data.overflowed;
        match event {
            Event::Alloc { size, overhead } => {
                add(&mut c.current_requested, size, bad);
                add(&mut c.total_allocated, size, bad);
                add(&mut c.live_blocks, 1, bad);
                add(&mut c.alloc_calls, 1, bad);
                add(&mut c.current_overhead, overhead, bad);
                add(&mut self.data.total_current_requested, size, bad);
                add(&mut self.data.current_overhead, overhead, bad);
            }
            Event::Free { size, overhead } => {
                sub(&mut c.current_requested, size, bad);
                add(&mut c.total_freed, size, bad);
                sub(&mut c.live_blocks, 1, bad);
                add(&mut c.dealloc_calls, 1, bad);
                sub(&mut c.current_overhead, overhead, bad);
                sub(&mut self.data.total_current_requested, size, bad);
                sub(&mut self.data.current_overhead, overhead, bad);
            }
            Event::Realloc { old, new } => {
                sub(&mut c.current_requested, old, bad);
                add(&mut c.current_requested, new, bad);
                add(&mut c.total_freed, old, bad);
                add(&mut c.total_allocated, new, bad);
                add(&mut c.realloc_calls, 1, bad);
                sub(&mut self.data.total_current_requested, old, bad);
                add(&mut self.data.total_current_requested, new, bad);
            }
        }
        c.peak_requested = c.peak_requested.max(c.current_requested);
        add(&mut self.data.event_sequence, 1, bad);
        if self.data.total_current_requested > self.data.global_peak_requested {
            self.data.global_peak_requested = self.data.total_current_requested;
            self.data.global_peak_overhead = self.data.current_overhead;
            self.data.peak_components = self.data.components.map(|c| c.current_requested);
        }
    }
    fn reset(&mut self) {
        self.data.global_peak_requested = self.data.total_current_requested;
        self.data.global_peak_overhead = self.data.current_overhead;
        for c in &mut self.data.components {
            c.peak_requested = c.current_requested;
        }
        self.data.peak_components = self.data.components.map(|c| c.current_requested);
        add(&mut self.data.event_sequence, 1, &mut self.data.overflowed);
    }
}
struct Telemetry {
    held: AtomicBool,
    data: UnsafeCell<Ledger>,
}
// SAFETY: every ledger access is guarded by held; references never escape with().
unsafe impl Sync for Telemetry {}
struct Guard<'a>(&'a AtomicBool);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
impl Telemetry {
    fn with<R>(&self, f: impl FnOnce(&mut Ledger) -> R) -> R {
        while self
            .held
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        let guard = Guard(&self.held);
        // SAFETY: held grants this thread exclusive access until guard is dropped.
        let result = f(unsafe { &mut *self.data.get() });
        drop(guard);
        result
    }
}
static LEDGER: Telemetry = Telemetry {
    held: AtomicBool::new(false),
    data: UnsafeCell::new(Ledger::new()),
};
static ACTIVE: AtomicBool = AtomicBool::new(false);
thread_local! {static CURRENT:Cell<usize>=const{Cell::new(Component::Unattributed as usize)};}
fn origin() -> usize {
    CURRENT
        .try_with(Cell::get)
        .unwrap_or(Component::Unattributed as usize)
}
/// True only after this process has observed a successful TrackingAllocator allocation.
/// Importing this library never installs a global allocator.
pub fn enabled() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
/// Set allocation origin on this thread. Drop restores the previous scope; unavailable
/// or inactive TLS becomes an explicit unattributed no-op, never a new allocation.
pub fn scope(component: Component) -> ScopeGuard {
    let previous = if enabled() {
        CURRENT.try_with(|c| c.replace(component as usize)).ok()
    } else {
        None
    };
    ScopeGuard {
        previous: previous.unwrap_or(0),
        active: previous.is_some(),
        _not_send: PhantomData,
    }
}
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = CURRENT.try_with(|c| c.set(self.previous));
        }
    }
}
/// Fixed-size allocation-free snapshot, serialized with ledger events. The peak is
/// linearized requested heap, not System-internal allocations or OS working set.
pub fn snapshot() -> Snapshot {
    LEDGER.with(|l| l.data)
}
/// Keep current/live and all cumulative counters, resetting only local/global peaks.
pub fn reset_peak_window() {
    if enabled() {
        LEDGER.with(Ledger::reset);
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    requested: usize,
    system_size: usize,
    system_align: usize,
    origin: usize,
}
fn combined(size: usize, align: usize) -> Option<(Layout, usize)> {
    if size == 0 {
        return None;
    }
    let align = align.max(std::mem::align_of::<Header>());
    let mask = align.checked_sub(1)?;
    let prefix = std::mem::size_of::<Header>().checked_add(mask)? & !mask;
    let total = prefix.checked_add(size)?;
    Some((Layout::from_size_align(total, align).ok()?, prefix))
}
impl<A: GlobalAlloc> TrackingAllocator<A> {
    unsafe fn allocate(&self, layout: Layout, zeroed: bool) -> *mut u8 {
        let Some((system, prefix)) = combined(layout.size(), layout.align()) else {
            return ptr::null_mut();
        };
        let tag = origin();
        // SAFETY: combined validated nonzero size, alignment and header/size arithmetic.
        let base = if zeroed {
            unsafe { self.inner.alloc_zeroed(system) }
        } else {
            unsafe { self.inner.alloc(system) }
        };
        if base.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: prefix/user region fit the underlying block. Header alignment is included
        // in system alignment and header stays wholly outside the user region.
        let user = unsafe { base.add(prefix) };
        let header = Header {
            requested: layout.size(),
            system_size: system.size(),
            system_align: system.align(),
            origin: tag,
        };
        unsafe {
            ptr::write(
                user.sub(std::mem::size_of::<Header>()).cast::<Header>(),
                header,
            )
        };
        LEDGER.with(|l| {
            l.event(
                tag,
                Event::Alloc {
                    size: layout.size() as u64,
                    overhead: prefix as u64,
                },
            )
        });
        ACTIVE.store(true, Ordering::Relaxed);
        user
    }
}
// SAFETY: the wrapper returns aligned pointers into unique blocks from A and stores
// its own layout/origin outside user bytes. Only its own headers are accessed, all
// calls to A occur outside the ledger lock, and bookkeeping cannot unwind/allocate.
unsafe impl<A: GlobalAlloc> GlobalAlloc for TrackingAllocator<A> {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        unsafe { self.allocate(l, false) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        unsafe { self.allocate(l, true) }
    }
    unsafe fn dealloc(&self, user: *mut u8, _layout: Layout) {
        // SAFETY: caller guarantees a live pointer returned by this allocator. Its private
        // header was written by allocate or a successful realloc.
        let h = unsafe { ptr::read(user.sub(std::mem::size_of::<Header>()).cast::<Header>()) };
        let prefix = h.system_size - h.requested;
        // SAFETY: header layout was validated before the underlying allocation.
        let l = unsafe { Layout::from_size_align_unchecked(h.system_size, h.system_align) };
        let base = unsafe { user.sub(prefix) };
        LEDGER.with(|ledger| {
            ledger.event(
                h.origin,
                Event::Free {
                    size: h.requested as u64,
                    overhead: prefix as u64,
                },
            )
        });
        // SAFETY: base/layout identify the same underlying live block returned by A.
        unsafe { self.inner.dealloc(base, l) };
    }
    unsafe fn realloc(&self, user: *mut u8, _layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: caller guarantees this is the live user pointer returned by this allocator.
        let old = unsafe { ptr::read(user.sub(std::mem::size_of::<Header>()).cast::<Header>()) };
        let Some((new, prefix)) = combined(new_size, old.system_align) else {
            return ptr::null_mut();
        };
        let old_prefix = old.system_size - old.requested;
        // SAFETY: old header preserves the validated original underlying layout, and the
        // new size is validated with the same alignment/prefix, preserving user alignment.
        let old_layout =
            unsafe { Layout::from_size_align_unchecked(old.system_size, old.system_align) };
        let base = unsafe { user.sub(old_prefix) };
        let next = unsafe { self.inner.realloc(base, old_layout, new.size()) };
        if next.is_null() {
            return ptr::null_mut();
        }
        let next_user = unsafe { next.add(prefix) };
        let h = Header {
            requested: new_size,
            system_size: new.size(),
            system_align: new.align(),
            origin: old.origin,
        };
        // SAFETY: unchanged prefix contains private header outside the valid user bytes.
        unsafe {
            ptr::write(
                next_user
                    .sub(std::mem::size_of::<Header>())
                    .cast::<Header>(),
                h,
            )
        };
        LEDGER.with(|l| {
            l.event(
                old.origin,
                Event::Realloc {
                    old: old.requested as u64,
                    new: new_size as u64,
                },
            )
        });
        next_user
    }
}
// Census operations are outside GlobalAlloc and have their own lock. Collector
// metadata and snapshots allocate only inside an ordinary Harness origin scope.
static CENSUS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());
pub fn census_set(key: &'static str, value: u64) {
    if enabled() {
        let _s = scope(Component::Harness);
        CENSUS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, value);
    }
}
pub fn census_snapshot() -> BTreeMap<&'static str, u64> {
    if !enabled() {
        return BTreeMap::new();
    }
    let _s = scope(Component::Harness);
    CENSUS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}
pub fn clear_census() {
    if enabled() {
        let _s = scope(Component::Harness);
        CENSUS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }
}
#[cfg(test)]
mod local_ledger_tests {
    use super::*;
    #[test]
    fn overflow_and_underflow_are_explicit_without_real_large_allocations() {
        let mut l = Ledger::new();
        l.event(
            0,
            Event::Alloc {
                size: u64::MAX,
                overhead: 0,
            },
        );
        l.event(
            0,
            Event::Alloc {
                size: 1,
                overhead: 0,
            },
        );
        assert!(l.data.overflowed);
        let mut l = Ledger::new();
        l.event(
            0,
            Event::Free {
                size: 1,
                overhead: 0,
            },
        );
        assert!(l.data.overflowed);
        let mut l = Ledger::new();
        l.event(
            COMPONENT_COUNT,
            Event::Alloc {
                size: 1,
                overhead: 0,
            },
        );
        assert!(l.data.overflowed);
    }
}
