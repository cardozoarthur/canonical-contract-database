use canonical_index::*;
fn entry(
    ns: u64,
    id: u64,
    revision: u64,
    sequence: u64,
    offset: u64,
    member: u32,
    deleted: bool,
) -> IndexEntry {
    IndexEntry {
        key: IndexKey {
            namespace: ns,
            id,
            revision,
        },
        location: Location {
            commit_sequence: sequence,
            frame_offset: offset,
            member,
            deleted,
        },
    }
}
#[test]
fn grouped_and_delta_preserve_sparse_identity_history_and_exact_stream_comparison() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut index = PrimaryIndex::new();
    for e in [
        entry(0, 1, 3, 4, 100, 1023, false),
        entry(0, 1, 8, 100, 30000, 1, true),
        entry(0, 9, 1, 1, 16, 0, false),
        entry(9, 1, 1, 1, 16, 2, false),
    ] {
        index.insert_retained(e).unwrap();
    }
    for format in [CacheFormat::Grouped, CacheFormat::Delta] {
        index.save_with_format(&p, 100, format).unwrap();
        assert_eq!(PrimaryIndex::load(&p, 100).unwrap(), index);
        assert_eq!(
            PrimaryIndex::validate_cache(&p, 100, &index).unwrap(),
            CacheValidation::Match
        );
    }
}
#[test]
fn legacy_stream_validation_does_not_require_loading_a_second_index() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut index = PrimaryIndex::new();
    index.insert(entry(0, 1, 1, 1, 16, 0, false)).unwrap();
    index.save(&p, 1).unwrap();
    assert_eq!(
        PrimaryIndex::validate_cache(&p, 1, &index).unwrap(),
        CacheValidation::Match
    );
    assert_eq!(
        PrimaryIndex::validate_cache(&p, 1, &PrimaryIndex::new()).unwrap(),
        CacheValidation::ContentMismatch
    );
}

fn crc(mut b: Vec<u8>) -> Vec<u8> {
    let h = crc32fast::hash(&b);
    b.extend_from_slice(&h.to_le_bytes());
    b
}
fn v3(layout: u8, n: u64, g: u64, body: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 32];
    b[..8].copy_from_slice(b"CBRIDX01");
    b[8..10].copy_from_slice(&3u16.to_le_bytes());
    b[10] = layout;
    b[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
    b[24..32].copy_from_slice(&n.to_le_bytes());
    b.extend_from_slice(&g.to_le_bytes());
    b.extend_from_slice(body);
    crc(b)
}
fn var(mut n: u64) -> Vec<u8> {
    let mut b = Vec::new();
    loop {
        let mut v = (n & 127) as u8;
        n >>= 7;
        if n > 0 {
            v |= 128;
        }
        b.push(v);
        if n == 0 {
            return b;
        }
    }
}
fn vars(nums: &[u64]) -> Vec<u8> {
    nums.iter().flat_map(|n| var(*n)).collect()
}
fn rejects(bytes: &[u8]) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bad.cbi");
    std::fs::write(&p, bytes).unwrap();
    assert!(PrimaryIndex::load(&p, u64::MAX).is_err());
    assert!(PrimaryIndex::validate_cache(&p, u64::MAX, &PrimaryIndex::new()).is_err());
}
#[test]
fn v3_golden_layouts_empty_sizes_namespace_zero_and_u64_boundaries() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut i = PrimaryIndex::new();
    i.insert_retained(entry(0, 1, 3, 4, 100, 1023, false))
        .unwrap();
    let mut body = Vec::new();
    for n in [0u64, 1, 1, 3, 4, 100] {
        body.extend_from_slice(&n.to_le_bytes());
    }
    body.extend_from_slice(&1023u32.to_le_bytes());
    body.extend_from_slice(&[0; 4]);
    i.save_with_format(&p, u64::MAX, CacheFormat::Grouped)
        .unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), v3(1, 1, 1, &body));
    i.save_with_format(&p, u64::MAX, CacheFormat::Delta)
        .unwrap();
    assert_eq!(
        std::fs::read(&p).unwrap(),
        v3(2, 1, 1, &[0, 1, 1, 3, 4, 100, 254, 15])
    );
    let mut maximum = PrimaryIndex::new();
    maximum
        .insert_retained(entry(
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            u64::MAX,
            1023,
            true,
        ))
        .unwrap();
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        maximum.save_with_format(&p, u64::MAX, format).unwrap();
        assert_eq!(PrimaryIndex::load(&p, u64::MAX).unwrap(), maximum);
        assert_eq!(
            PrimaryIndex::validate_cache(&p, u64::MAX, &maximum).unwrap(),
            CacheValidation::Match
        );
        let empty = PrimaryIndex::new();
        empty.save_with_format(&p, 0, format).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().len(),
            if format == CacheFormat::Legacy {
                36
            } else {
                44
            }
        );
        assert!(PrimaryIndex::load(&p, 0).unwrap().is_empty());
    }
}
#[test]
fn v3_every_byte_change_every_cut_and_trailing_are_rejected() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut i = PrimaryIndex::new();
    i.insert_retained(entry(0, 1, 3, 4, 100, 1023, false))
        .unwrap();
    i.insert_retained(entry(0, 1, 8, 10, 1000, 0, true))
        .unwrap();
    for format in [CacheFormat::Grouped, CacheFormat::Delta] {
        i.save_with_format(&p, u64::MAX, format).unwrap();
        let bytes = std::fs::read(&p).unwrap();
        for n in 0..bytes.len() {
            let mut b = bytes.clone();
            b[n] ^= 1;
            rejects(&b);
            rejects(&bytes[..n]);
        }
        let mut b = bytes.clone();
        b.push(0);
        rejects(&b);
    }
}
#[test]
fn v3_crc_valid_hostile_counts_reserved_flags_and_noncanonical_varints_are_rejected() {
    for (n, g, body) in [
        (MAX_INDEX_ENTRIES + 1, 1, vars(&[0, 1, 1, 1, 1, 16, 0])),
        (1, 0, vec![]),
        (0, 1, vec![]),
        (1, 2, vec![]),
        (1, 1, vars(&[0, 1, 0])),
        (1, 1, vars(&[0, 1, 2])),
        (1, 1, vars(&[0, 1, 1, 1, 1, 16, 2048])),
    ] {
        rejects(&v3(2, n, g, &body));
    }
    rejects(&v3(1, MAX_INDEX_ENTRIES, MAX_INDEX_ENTRIES, &[]));
    let valid = vars(&[0, 1, 1, 1, 1, 16, 0]);
    for bad in [
        vec![128; 10],
        vec![255; 9].into_iter().chain([2]).collect(),
        vec![129, 0],
        vec![128; 11],
    ] {
        let mut body = bad;
        body.extend_from_slice(&valid[1..]);
        rejects(&v3(2, 1, 1, &body));
    }
    // Invalid v3 reserved header bytes and unsupported layout, all checksum-valid.
    let good = v3(2, 1, 1, &valid);
    for (pos, value) in [(10, 0), (10, 3), (11, 1), (15, 1)] {
        let mut b = good[..good.len() - 4].to_vec();
        b[pos] = value;
        rejects(&crc(b));
    }
    // Grouped reserved bytes/member/deleted and missing/extra group counts.
    let mut body = Vec::new();
    for n in [0u64, 1, 1, 1, 1, 16] {
        body.extend_from_slice(&n.to_le_bytes());
    }
    body.extend_from_slice(&[0; 8]);
    for pos in [24 + 28, 24 + 29, 24 + 30, 24 + 31] {
        let mut b = body.clone();
        b[pos] = 2;
        rejects(&v3(1, 1, 1, &b));
    }
    let mut b = body.clone();
    b[24 + 24..24 + 28].copy_from_slice(&1024u32.to_le_bytes());
    rejects(&v3(1, 1, 1, &b));
    // Bounds are checked before allocating entries, independently of physical size.
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("large.cbi");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(MAX_INDEX_BYTES + 1).unwrap();
    drop(f);
    assert!(matches!(
        PrimaryIndex::validate_cache(&p, 0, &PrimaryIndex::new()),
        Err(canonical_core::Error::TooLarge)
    ));
}
#[test]
fn delta_checked_addition_order_and_positive_deltas_are_enforced() {
    // Each array begins with one group header and two revisions. Revision/sequence/offset overflow independently.
    for field in 0..3 {
        let mut first = [1, 1, 16];
        first[field] = u64::MAX;
        let mut nums = vec![0, 1, 2];
        nums.extend(first);
        nums.push(0);
        nums.extend([1, 1, 1, 0]);
        rejects(&v3(2, 2, 1, &vars(&nums)));
    }
    for field in 0..3 {
        let mut delta = [1, 1, 1];
        delta[field] = 0;
        let mut nums = vec![0, 1, 2, 1, 1, 16, 0];
        nums.extend(delta);
        nums.push(0);
        rejects(&v3(2, 2, 1, &vars(&nums)));
    }
    // Namespace and ID delta overflow, duplicate group, group count/entry sum mismatch.
    for nums in [
        vec![u64::MAX, 1, 1, 1, 1, 16, 0, 1, 1, 1, 1, 1, 16, 0],
        vec![0, u64::MAX, 1, 1, 1, 16, 0, 0, 1, 1, 1, 1, 16, 0],
        vec![0, 1, 1, 1, 1, 16, 0, 0, 0, 1, 1, 1, 16, 0],
        vec![0, 1, 1, 1, 1, 16, 0],
    ] {
        rejects(&v3(2, 2, 2, &vars(&nums)));
    }
}
#[test]
fn stream_mismatch_is_exact_and_does_not_short_circuit_final_crc_or_structure() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut i = PrimaryIndex::new();
    i.insert_retained(entry(0, 1, 3, 4, 100, 1, false)).unwrap();
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        for other in [
            entry(1, 1, 3, 4, 100, 1, false),
            entry(0, 2, 3, 4, 100, 1, false),
            entry(0, 1, 4, 4, 100, 1, false),
            entry(0, 1, 3, 5, 100, 1, false),
            entry(0, 1, 3, 4, 101, 1, false),
            entry(0, 1, 3, 4, 100, 2, false),
            entry(0, 1, 3, 4, 100, 1, true),
        ] {
            let mut modified = PrimaryIndex::new();
            modified.insert_retained(other).unwrap();
            modified.save_with_format(&p, 100, format).unwrap();
            assert_eq!(
                PrimaryIndex::validate_cache(&p, 100, &i).unwrap(),
                CacheValidation::ContentMismatch
            );
        }
        i.save_with_format(&p, 100, format).unwrap();
        let mut b = std::fs::read(&p).unwrap();
        let last = b.len() - 1;
        b[last] ^= 1;
        std::fs::write(&p, &b).unwrap();
        assert!(
            PrimaryIndex::validate_cache(&p, 100, &PrimaryIndex::new()).is_err(),
            "mismatched count cannot skip CRC"
        );
        assert!(matches!(
            PrimaryIndex::validate_cache(&p, 99, &i),
            Err(canonical_core::Error::Conflict)
        ));
    }
    // First entry mismatches expected, later entry has malformed zero revision; valid CRC still must fail.
    let b = v3(2, 2, 1, &vars(&[0, 1, 2, 1, 1, 16, 0, 0, 1, 1, 0]));
    std::fs::write(&p, b).unwrap();
    assert!(PrimaryIndex::validate_cache(&p, u64::MAX, &PrimaryIndex::new()).is_err());
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static COST:Cell<(usize,usize,usize)>=const{Cell::new((0,0,0))};}
struct Counting;
fn record(n: usize) {
    TRACK.with(|t| {
        if t.get() {
            COST.with(|c| {
                let (a, b, m) = c.get();
                c.set((a + 1, b + n, m.max(n)));
            });
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        record(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        record(n);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn measure<T>(f: impl FnOnce() -> T) -> (T, (usize, usize, usize)) {
    COST.with(|c| c.set((0, 0, 0)));
    TRACK.with(|t| t.set(true));
    let v = f();
    TRACK.with(|t| t.set(false));
    (v, COST.with(Cell::get))
}
#[test]
fn stream_validation_allocations_are_bounded_independently_of_entry_count() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut small = PrimaryIndex::new();
    small.insert(entry(0, 1, 1, 1, 16, 0, false)).unwrap();
    let mut large = PrimaryIndex::new();
    for id in 1..=10000 {
        large
            .insert(entry(0, id, 1, id, 16 + id * 64, 0, false))
            .unwrap();
    }
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        small.save_with_format(&p, 10000, format).unwrap();
        let (result, a) = measure(|| PrimaryIndex::validate_cache(&p, 10000, &small).unwrap());
        assert_eq!(result, CacheValidation::Match);
        large.save_with_format(&p, 10000, format).unwrap();
        let (result, b) = measure(|| PrimaryIndex::validate_cache(&p, 10000, &large).unwrap());
        assert_eq!(result, CacheValidation::Match);
        assert_eq!(a, b, "no allocation scales with entries");
        assert!(
            b.2 <= 8192,
            "largest allocation is at most the reader buffer"
        );
    }
}
#[test]
fn delta_varint_thresholds_and_multiple_namespace_resets_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut index = PrimaryIndex::new();
    for (ns, id) in [(0, 127), (0, 128), (127, 1), (128, 1), (u64::MAX, u64::MAX)] {
        for revision in 1..=128 {
            index
                .insert_retained(entry(
                    ns,
                    id,
                    revision,
                    revision * 128,
                    16 + revision * 128,
                    if revision % 2 == 0 { 64 } else { 63 },
                    revision % 2 != 0,
                ))
                .unwrap();
        }
    }
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        index.save_with_format(&p, 16384, format).unwrap();
        assert_eq!(PrimaryIndex::load(&p, 16384).unwrap(), index);
        assert_eq!(
            PrimaryIndex::validate_cache(&p, 16384, &index).unwrap(),
            CacheValidation::Match
        );
    }
}
