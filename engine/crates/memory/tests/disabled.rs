use canonical_memory::*;
#[test]
fn importing_library_does_not_install_allocator_or_enable_collection() {
    assert!(!enabled());
    let before = snapshot();
    let _s = scope(Component::QueryRows);
    let data = vec![1u8; 1024];
    census_set("ignored", data.len() as u64);
    reset_peak_window();
    assert!(census_snapshot().is_empty());
    assert_eq!(before, snapshot());
    assert!(!enabled());
}
