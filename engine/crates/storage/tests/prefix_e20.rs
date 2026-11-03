use canonical_core::Error;
use canonical_storage::{RewriteStage, Store};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    fs::OpenOptions,
    io::{Seek, SeekFrom, Write},
    sync::Arc,
};

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
struct Count;
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ACTIVE.with(|active| {
            if active.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ACTIVE.with(|active| {
            if active.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Count = Count;

#[test]
fn prefix_entry_clones_one_handle_without_allocating_a_snapshot_list() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    for value in 0..32u8 {
        store.append(&[value; 128]).unwrap();
    }
    let stats = store.io_stats();
    ALLOCATIONS.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
    let entry = store.prefix_entry(17);
    ACTIVE.with(|active| active.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    assert_eq!(
        allocations, 0,
        "a point prefix handle must not allocate the whole snapshot list"
    );
    let entry = entry.unwrap();
    assert_eq!(entry.payload, vec![16; 128]);
    let all = store.snapshot();
    assert!(Arc::ptr_eq(&entry, &all[16]));
    assert_eq!(store.io_stats(), stats);
    assert!(store.prefix_entry(0).is_none());
    assert!(store.prefix_entry(33).is_none());
    assert!(store.prefix_entry(u64::MAX).is_none());
}

#[test]
fn held_prefix_survives_generation_activation_and_late_failure() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    store.append(b"original one").unwrap();
    store.append(b"original two").unwrap();
    let original = store.prefix_entry(1).unwrap();
    let error = store
        .rewrite_with_hook(
            &[b"rewritten one".to_vec(), b"rewritten two".to_vec()],
            |stage| {
                if stage == RewriteStage::Activated {
                    Err(Error::InvalidInput("E20 injected activation failure"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
    assert!(matches!(error, Error::InvalidInput(_)));
    assert!(store.ensure_healthy().is_err());
    assert_eq!(original.payload, b"original one");
    let current = store.prefix_entry(1).unwrap();
    assert_eq!(current.payload, b"rewritten one");
    assert!(!Arc::ptr_eq(&original, &current));
    drop(store);
    assert_eq!(original.payload, b"original one");
}

#[test]
fn prefix_handle_does_not_claim_to_verify_a_tampered_disk_frame() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path()).unwrap();
    store.append(b"validated original").unwrap();
    let original = store.prefix_entry(1).unwrap();
    let offset = store.frame_offset(1).unwrap();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.active_segment_path())
        .unwrap();
    file.seek(SeekFrom::Start(
        offset + canonical_codec::FRAME_HEADER as u64,
    ))
    .unwrap();
    file.write_all(&[0x55]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let stats = store.io_stats();
    assert!(Arc::ptr_eq(&original, &store.prefix_entry(1).unwrap()));
    assert_eq!(store.io_stats(), stats);
    assert!(matches!(
        store.read_verified_frame_at(offset),
        Err(Error::Corruption { .. })
    ));
    assert_eq!(store.io_stats().point_reads, stats.point_reads + 1);
    assert!(store.ensure_healthy().is_err());
    assert_eq!(
        store.prefix_entry(1).unwrap().payload,
        b"validated original"
    );
}
