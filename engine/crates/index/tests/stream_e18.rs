use canonical_index::*;
fn entry(id: u64, rev: u64, seq: u64) -> IndexEntry {
    IndexEntry {
        key: IndexKey {
            namespace: 0,
            id,
            revision: rev,
        },
        location: Location {
            commit_sequence: seq,
            frame_offset: 16 + seq * 64,
            member: 1023,
            deleted: rev > 1,
        },
    }
}
#[test]
fn stream_preserves_legacy_bytes_from_independent_layout_and_all_formats() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("stream.cbi");
    for (entries, version) in [
        (vec![], 1u16),
        (vec![entry(1, 1, 1), entry(1, 2, 2), entry(2, 1, 1)], 1),
        (vec![entry(1, 3, 1), entry(1, 7, 2), entry(2, 1, 1)], 2),
        (vec![entry(1, 1, 1), entry(1, 7, 2)], 2),
    ] {
        let mut expected = vec![0u8; 32];
        expected[..8].copy_from_slice(b"CBRIDX01");
        expected[8..10].copy_from_slice(&version.to_le_bytes());
        expected[16..24].copy_from_slice(&2u64.to_le_bytes());
        expected[24..32].copy_from_slice(&(entries.len() as u64).to_le_bytes());
        for e in &entries {
            for n in [
                e.key.namespace,
                e.key.id,
                e.key.revision,
                e.location.commit_sequence,
                e.location.frame_offset,
            ] {
                expected.extend_from_slice(&n.to_le_bytes());
            }
            expected.extend_from_slice(&e.location.member.to_le_bytes());
            expected.push(u8::from(e.location.deleted));
            expected.extend_from_slice(&[0; 3]);
        }
        let crc = crc32fast::hash(&expected);
        expected.extend_from_slice(&crc.to_le_bytes());
        let mut index = PrimaryIndex::new();
        for &e in &entries {
            index.insert_retained(e).unwrap();
        }
        for format in [
            CacheFormat::Legacy,
            CacheFormat::Grouped,
            CacheFormat::Delta,
        ] {
            save_entries_with_format(
                &p,
                2,
                entries.len() as u64,
                format,
                entries.iter().copied().map(Ok),
            )
            .unwrap();
            if format == CacheFormat::Legacy {
                assert_eq!(std::fs::read(&p).unwrap(), expected);
            }
            assert_eq!(PrimaryIndex::load(&p, 2).unwrap(), index);
            assert_eq!(
                validate_cache_entries(
                    &p,
                    2,
                    entries.len() as u64,
                    entries.iter().copied().map(Ok)
                )
                .unwrap(),
                CacheValidation::Match
            );
            let other = d.path().join("old.cbi");
            index.save_with_format(&other, 2, format).unwrap();
            assert_eq!(std::fs::read(&p).unwrap(), std::fs::read(other).unwrap());
        }
    }
}

#[test]
fn malformed_sources_counts_and_errors_preserve_old_published_cache() {
    use canonical_core::Error;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let valid = [entry(1, 1, 1), entry(1, 2, 2)];
    save_entries_with_format(&p, 2, 2, CacheFormat::Legacy, valid.into_iter().map(Ok)).unwrap();
    let old = std::fs::read(&p).unwrap();
    let mut cases = vec![vec![valid[1], valid[0]], vec![valid[0], valid[0]]];
    for mutation in 0..8 {
        let mut bad = valid;
        match mutation {
            0 => bad[0].key.id = 0,
            1 => bad[0].key.revision = 0,
            2 => bad[0].location.commit_sequence = 0,
            3 => bad[0].location.frame_offset = 15,
            4 => bad[0].location.member = 1024,
            5 => bad[0].location.commit_sequence = 3,
            6 => bad[1].location.commit_sequence = 1,
            _ => bad[1].location.frame_offset = bad[0].location.frame_offset,
        };
        cases.push(bad.to_vec());
    }
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        for bad in &cases {
            assert!(
                save_entries_with_format(
                    &p,
                    2,
                    bad.len() as u64,
                    format,
                    bad.iter().copied().map(Ok)
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&p).unwrap(), old);
            assert!(
                validate_cache_entries(&p, 2, bad.len() as u64, bad.iter().copied().map(Ok))
                    .is_err()
            );
        }
        for count in [0, 1, 3, MAX_INDEX_ENTRIES + 1] {
            assert!(
                save_entries_with_format(&p, 2, count, format, valid.into_iter().map(Ok)).is_err()
            );
            assert_eq!(std::fs::read(&p).unwrap(), old);
        }
        let failure = (0..2).map(|i| {
            if i == 0 {
                Ok(entry(1, 1, 1))
            } else {
                Err(Error::Conflict)
            }
        });
        assert!(matches!(
            save_entries_with_format(&p, 2, 2, format, failure),
            Err(Error::Conflict)
        ));
        assert_eq!(std::fs::read(&p).unwrap(), old);
    }
}

struct LateError<'a> {
    entries: &'a [IndexEntry],
    at: usize,
    original: bool,
}
impl Clone for LateError<'_> {
    fn clone(&self) -> Self {
        Self {
            entries: self.entries,
            at: self.at,
            original: false,
        }
    }
}
impl Iterator for LateError<'_> {
    type Item = canonical_core::Result<IndexEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        let e = *self.entries.get(self.at)?;
        self.at += 1;
        Some(if self.original && self.at == 2 {
            Err(canonical_core::Error::Conflict)
        } else {
            Ok(e)
        })
    }
}
#[test]
fn source_error_after_preflight_never_publishes_and_validation_still_checks_footer() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let valid = [entry(1, 1, 1), entry(1, 2, 2)];
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        save_entries_with_format(&p, 2, 2, format, valid.into_iter().map(Ok)).unwrap();
        let old = std::fs::read(&p).unwrap();
        let source = || LateError {
            entries: &valid,
            at: 0,
            original: true,
        };
        assert!(matches!(
            save_entries_with_format(&p, 2, 2, format, source()),
            Err(canonical_core::Error::Conflict)
        ));
        assert_eq!(std::fs::read(&p).unwrap(), old);
        assert!(
            !d.path().join("cache.cbi.tmp").exists(),
            "encoding source failure must clean its unpublished temporary"
        );
        assert!(matches!(
            validate_cache_entries(&p, 2, 2, source()),
            Err(canonical_core::Error::Conflict)
        ));
        let mut broken = old;
        let n = broken.len();
        broken[n - 1] ^= 1;
        std::fs::write(&p, broken).unwrap();
        assert!(matches!(
            validate_cache_entries(&p, 2, 2, source()),
            Err(canonical_core::Error::Corruption { .. })
        ));
    }
}
#[test]
fn mismatch_does_not_hide_late_crc_structural_failure_or_extra_expected_entries() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let expected = [entry(1, 1, 1), entry(1, 2, 2)];
    let mut wrong = expected;
    wrong[0].location.member = 1;
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        save_entries_with_format(&p, 2, 2, format, wrong.into_iter().map(Ok)).unwrap();
        assert_eq!(
            validate_cache_entries(&p, 2, 2, expected.into_iter().map(Ok)).unwrap(),
            CacheValidation::ContentMismatch
        );
        let mut bad = std::fs::read(&p).unwrap();
        let n = bad.len();
        bad[n - 1] ^= 1;
        std::fs::write(&p, bad).unwrap();
        assert!(matches!(
            validate_cache_entries(&p, 2, 2, expected.into_iter().map(Ok)),
            Err(canonical_core::Error::Corruption { .. })
        ));
        save_entries_with_format(&p, 2, 1, format, wrong[..1].iter().copied().map(Ok)).unwrap();
        assert_eq!(
            validate_cache_entries(&p, 2, 2, expected.into_iter().map(Ok)).unwrap(),
            CacheValidation::ContentMismatch
        );
    }
    save_entries_with_format(&p, 2, 2, CacheFormat::Legacy, wrong.into_iter().map(Ok)).unwrap();
    let mut bad = std::fs::read(&p).unwrap();
    bad[32 + 48 + 40..32 + 48 + 44].copy_from_slice(&1024u32.to_le_bytes());
    let n = bad.len();
    let crc = crc32fast::hash(&bad[..n - 4]);
    bad[n - 4..].copy_from_slice(&crc.to_le_bytes());
    std::fs::write(&p, bad).unwrap();
    assert!(matches!(
        validate_cache_entries(&p, 2, 2, expected.into_iter().map(Ok)),
        Err(canonical_core::Error::Corruption { .. })
    ));
}

#[test]
fn standalone_validation_keeps_old_reader_first_error_precedence() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let mut disk = PrimaryIndex::new();
    disk.insert(entry(1, 1, 1)).unwrap();
    disk.save(&p, 1).unwrap();
    let mut expected = PrimaryIndex::new();
    expected.insert(entry(1, 1, 2)).unwrap();
    assert_eq!(
        PrimaryIndex::validate_cache(&p, 1, &expected).unwrap(),
        CacheValidation::ContentMismatch
    );
    assert!(matches!(
        PrimaryIndex::validate_cache(&d.path().join("missing"), 1, &expected),
        Err(canonical_core::Error::Io(_))
    ));
    assert!(validate_cache_entries(&p, 1, 1, expected.entries().into_iter().map(Ok)).is_err());
}

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static ALLOC_BYTES:Cell<usize>=const{Cell::new(0)};}
struct Counting;
fn allocated(n: usize) {
    TRACK.with(|on| {
        if on.get() {
            ALLOC_BYTES.with(|b| b.set(b.get() + n));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        allocated(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        allocated(n);
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn allocation_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOC_BYTES.with(|b| b.set(0));
    TRACK.with(|on| on.set(true));
    let value = f();
    TRACK.with(|on| on.set(false));
    (value, ALLOC_BYTES.with(Cell::get))
}
#[test]
fn repeatable_source_does_not_materialize_a_tree_or_entry_vector() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("stream.cbi");
    for n in [1, 10000] {
        for format in [
            CacheFormat::Legacy,
            CacheFormat::Grouped,
            CacheFormat::Delta,
        ] {
            let source = || (1..=n).map(|id| Ok(entry(id, 1, 1)));
            let (saved, save_bytes) =
                allocation_bytes(|| save_entries_with_format(&p, 1, n, format, source()));
            saved.unwrap();
            let (checked, read_bytes) =
                allocation_bytes(|| validate_cache_entries(&p, 1, n, source()));
            assert_eq!(checked.unwrap(), CacheValidation::Match);
            eprintln!(
                "stream entries={n} format={format:?} allocation_requested save={save_bytes}B validate={read_bytes}B"
            );
            assert!(
                save_bytes < 65536,
                "saving materialized {save_bytes} bytes for {n}entries"
            );
            assert!(
                read_bytes < 65536,
                "validation materialized {read_bytes} bytes for {n}entries"
            );
        }
    }
}

#[derive(Clone)]
struct UntouchedSource;
impl Iterator for UntouchedSource {
    type Item = canonical_core::Result<IndexEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        panic!("count mismatch must not inspect authority source")
    }
}
#[test]
fn authority_larger_than_cache_limit_keeps_reader_first_count_mismatch_without_materializing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let count = MAX_INDEX_ENTRIES + 1;
    assert!(matches!(
        validate_cache_entries(&p, 1, count, UntouchedSource),
        Err(canonical_core::Error::Io(_))
    ));
    save_entries_with_format(
        &p,
        1,
        1,
        CacheFormat::Legacy,
        std::iter::once(entry(1, 1, 1)).map(Ok),
    )
    .unwrap();
    assert_eq!(
        validate_cache_entries(&p, 1, count, UntouchedSource).unwrap(),
        CacheValidation::ContentMismatch
    );
    let mut bad = std::fs::read(&p).unwrap();
    let n = bad.len();
    bad[n - 1] ^= 1;
    std::fs::write(&p, bad).unwrap();
    assert!(matches!(
        validate_cache_entries(&p, 1, count, UntouchedSource),
        Err(canonical_core::Error::Corruption { .. })
    ));
}

#[derive(Clone)]
struct CountedSource {
    next: u64,
    end: u64,
    calls: std::rc::Rc<Cell<u64>>,
}
impl Iterator for CountedSource {
    type Item = canonical_core::Result<IndexEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        self.calls.set(self.calls.get() + 1);
        if self.next > self.end {
            return None;
        }
        let id = self.next;
        self.next += 1;
        Some(Ok(entry(id, 1, 1)))
    }
}
#[test]
fn bounded_size_proof_does_not_encode_an_extra_dry_pass() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cache.cbi");
    let n = 1000;
    for format in [
        CacheFormat::Legacy,
        CacheFormat::Grouped,
        CacheFormat::Delta,
    ] {
        let calls = std::rc::Rc::new(Cell::new(0));
        let source = CountedSource {
            next: 1,
            end: n,
            calls: calls.clone(),
        };
        save_entries_with_format(&p, 1, n, format, source).unwrap();
        assert_eq!(PrimaryIndex::load(&p, 1).unwrap().len(), n as usize);
        eprintln!(
            "format={format:?} entries={n} source_next_calls={}",
            calls.get()
        );
        assert!(
            calls.get() <= 3 * n + 4,
            "format{format:?} visited source {} times for {n}entries; only inspect+encoding/groupahead needed",
            calls.get()
        );
    }
}
