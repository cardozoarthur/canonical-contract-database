use canonical_registry::*;
fn fixture() -> (tempfile::TempDir, Registry) {
    fixture_n(16)
}
fn fixture_n(count: u64) -> (tempfile::TempDir, Registry) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 0,
        name: "history".into(),
        level: Level::Global,
    })
    .unwrap();
    let sr = SchemaRef {
        namespace: 0,
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
    r.create_batch(
        [1; 16],
        &(1..=count)
            .map(|id| NewRecord {
                namespace: 0,
                id,
                schema: sr,
                values: vec![Some(Value::Bytes(vec![id as u8; 4096]))],
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    (dir, r)
}
#[test]
fn selective_and_batch_materialize_only_requested_members_in_original_order() {
    let (_d, mut r) = fixture();
    let expected = r.history(0, 8)[0].clone();
    let request = LookupRequest {
        namespace: 0,
        id: 8,
        revision: Some(1),
    };
    let selected = r
        .lookup_many(&[request], HistoryReadMode::Selective)
        .unwrap();
    assert_eq!(selected.results[0].revision, Some(expected.clone()));
    assert_eq!(selected.metrics.members_decoded, 1);
    assert_eq!(selected.metrics.members_visited, 16);
    let reference = r
        .lookup_many(&[request], HistoryReadMode::ReferenceFull)
        .unwrap();
    assert_eq!(reference.results[0].revision, Some(expected.clone()));
    assert_eq!(reference.metrics.members_decoded, 16);
    let request2 = LookupRequest { id: 2, ..request };
    let batch = r
        .lookup_many(&[request, request2, request], HistoryReadMode::Batch)
        .unwrap();
    assert_eq!(batch.results[0].revision, Some(expected.clone()));
    assert_eq!(batch.results[1].revision.as_ref().unwrap().record.id, 2);
    assert_eq!(batch.results[2].revision, Some(expected));
    assert_eq!(batch.metrics.physical_frame_reads, 1);
    assert_eq!(batch.metrics.members_decoded, 2);
    assert_eq!(batch.metrics.members_visited, 16);
    assert_eq!(batch.metrics.output_records, 3);
}
fn tamper_sibling(dir: &std::path::Path, needle: u8) {
    let path = dir.join("segments/00000001.cbr");
    let mut bytes = std::fs::read(&path).unwrap();
    let mut at = 16;
    while at < bytes.len() {
        let (seq, payload, total) =
            canonical_codec::decode_frame_ref(&bytes[at..], at as u64).unwrap();
        if let Some(pos) = payload
            .windows(64)
            .position(|w| w.iter().all(|v| *v == needle))
        {
            let mut edited = payload.to_vec();
            edited[pos] ^= 1;
            let new = canonical_codec::encode_frame(seq, &edited).unwrap();
            assert_eq!(new.len(), total);
            bytes[at..at + total].copy_from_slice(&new);
            assert!(canonical_codec::decode_frame_ref(&bytes[at..], at as u64).is_ok());
            std::fs::write(path, bytes).unwrap();
            return;
        }
        at += total;
    }
    panic!("test payload missing");
}
#[test]
fn crc_correct_unselected_sibling_is_rejected_in_all_modes_and_poison_is_sticky() {
    for mode in [
        HistoryReadMode::ReferenceFull,
        HistoryReadMode::Selective,
        HistoryReadMode::Batch,
    ] {
        let (d, mut r) = fixture();
        tamper_sibling(d.path(), 16);
        let req = LookupRequest {
            namespace: 0,
            id: 8,
            revision: Some(1),
        };
        assert!(matches!(
            r.lookup_many(&[req], mode),
            Err(canonical_core::Error::Corruption { .. })
        ));
        assert_eq!(r.storage_stats().point_reads, 1);
        assert!(matches!(
            r.lookup_many(&[], mode),
            Err(canonical_core::Error::Poisoned)
        ));
        assert!(matches!(
            r.lookup(0, 0, None),
            Err(canonical_core::Error::Poisoned)
        ));
        assert!(matches!(
            r.flush_index(),
            Err(canonical_core::Error::Poisoned)
        ));
    }
}
#[test]
fn modes_preserve_record_changes_revision_tombstone_restore_and_compacted_ordinals() {
    for mode in [
        HistoryReadMode::ReferenceFull,
        HistoryReadMode::Selective,
        HistoryReadMode::Batch,
    ] {
        let (_d, mut r) = fixture();
        let sr = r.record(0, 1).unwrap().schema;
        r.create(0, 100, sr, &[Some(Value::Bytes(vec![100; 100]))])
            .unwrap();
        r.commit(
            [2; 16],
            &[
                Mutation::Update {
                    record: NewRecord {
                        namespace: 0,
                        id: 1,
                        schema: sr,
                        values: vec![Some(Value::Bytes(vec![31; 100]))],
                    },
                    expected_revision: 1,
                },
                Mutation::Delete {
                    namespace: 0,
                    id: 2,
                    expected_revision: 1,
                },
            ],
        )
        .unwrap();
        r.restore(
            NewRecord {
                namespace: 0,
                id: 2,
                schema: sr,
                values: vec![Some(Value::Bytes(vec![32; 100]))],
            },
            2,
        )
        .unwrap();
        r.delete(0, 3, 1).unwrap();
        let requests = [
            LookupRequest {
                namespace: 0,
                id: 100,
                revision: Some(1),
            },
            LookupRequest {
                namespace: 0,
                id: 1,
                revision: Some(2),
            },
            LookupRequest {
                namespace: 0,
                id: 2,
                revision: Some(2),
            },
            LookupRequest {
                namespace: 0,
                id: 2,
                revision: Some(3),
            },
            LookupRequest {
                namespace: 0,
                id: 3,
                revision: Some(2),
            },
            LookupRequest {
                namespace: 0,
                id: 3,
                revision: None,
            },
        ];
        let expected = requests
            .iter()
            .map(|q| {
                q.revision
                    .and_then(|rev| r.revision(q.namespace, q.id, rev).cloned())
            })
            .collect::<Vec<_>>();
        let out = r.lookup_many(&requests, mode).unwrap();
        assert_eq!(
            out.results
                .iter()
                .map(|o| o.revision.clone())
                .collect::<Vec<_>>(),
            expected
        );
        // Compaction prunes the first member of the original batch while later member slots stay physical.
        r.compact(Retention::KeepLast(1)).unwrap();
        assert_eq!(r.revision_status(0, 1, 1), RevisionStatus::Pruned);
        let reqs = [
            LookupRequest {
                namespace: 0,
                id: 8,
                revision: Some(1),
            },
            LookupRequest {
                namespace: 0,
                id: 16,
                revision: Some(1),
            },
            LookupRequest {
                namespace: 0,
                id: 1,
                revision: Some(2),
            },
        ];
        let before = reqs
            .iter()
            .map(|q| {
                r.revision(q.namespace, q.id, q.revision.unwrap())
                    .unwrap()
                    .clone()
            })
            .collect::<Vec<_>>();
        let out = r.lookup_many(&reqs, mode).unwrap();
        assert_eq!(
            out.results
                .into_iter()
                .map(|o| o.revision.unwrap())
                .collect::<Vec<_>>(),
            before
        );
        let stats = r.storage_stats();
        assert!(matches!(
            r.lookup_many(
                &[LookupRequest {
                    namespace: 0,
                    id: 1,
                    revision: Some(1)
                }],
                mode
            ),
            Err(canonical_core::Error::HistoryPruned)
        ));
        assert_eq!(r.storage_stats(), stats);
    }
}
#[test]
fn bounds_missing_empty_duplicates_and_shared_metric_attribution_are_explicit() {
    let (_d, mut r) = fixture();
    let missing = LookupRequest {
        namespace: 0,
        id: 0,
        revision: Some(0),
    };
    let before = r.storage_stats();
    let empty = r.lookup_many(&[], HistoryReadMode::Batch).unwrap();
    assert!(empty.results.is_empty());
    assert_eq!(empty.metrics, HistoryReadMetrics::default());
    let out = r.lookup_many(&[missing], HistoryReadMode::Batch).unwrap();
    assert!(out.results[0].revision.is_none());
    assert_eq!(r.storage_stats(), before);
    assert!(matches!(
        r.lookup_many(&vec![missing; 1025], HistoryReadMode::Batch),
        Err(canonical_core::Error::TooLarge)
    ));
    assert_eq!(r.storage_stats(), before);
    let req = LookupRequest {
        namespace: 0,
        id: 8,
        revision: Some(1),
    };
    let out = r.lookup_many(&[req, req], HistoryReadMode::Batch).unwrap();
    assert_eq!(out.results[0].metrics.point_reads, 1);
    assert_eq!(out.results[1].metrics.point_reads, 0);
    assert_eq!(out.results[0].metrics.decoded_members, 1);
    assert_eq!(out.results[1].metrics.decoded_members, 0);
    assert_eq!(out.metrics.members_decoded, 1);
    assert_eq!(out.metrics.output_records, 2);
    assert_eq!(out.metrics.header_crc_bytes_verified, 20);
    assert_eq!(
        out.metrics.payload_crc_bytes_verified,
        out.metrics.physical_bytes_read - 28
    );
    assert_eq!(
        out.metrics.prefix_payload_bytes_compared,
        out.metrics.payload_crc_bytes_verified
    );
    assert_eq!(out.results[0].metrics.frames_scanned, 0);
    // Per-call reuse never authorizes the next call without a physical read.
    let before = r.storage_stats();
    r.lookup_many(&[req, req], HistoryReadMode::Batch).unwrap();
    assert_eq!(r.storage_stats().point_reads - before.point_reads, 1);
    let sr = r.record(0, 1).unwrap().schema;
    r.create(0, 100, sr, &[Some(Value::Bytes(vec![8; 65536]))])
        .unwrap();
    let large = LookupRequest {
        namespace: 0,
        id: 100,
        revision: Some(1),
    };
    let before = r.storage_stats();
    assert!(matches!(
        r.lookup_many(&vec![large; 257], HistoryReadMode::Batch),
        Err(canonical_core::Error::TooLarge)
    ));
    assert_eq!(r.storage_stats(), before);
}
#[test]
fn physical_failure_in_second_frame_never_returns_partial_results() {
    let (d, mut r) = fixture();
    let sr = r.record(0, 1).unwrap().schema;
    r.create(0, 100, sr, &[Some(Value::Bytes(vec![100; 100]))])
        .unwrap();
    tamper_sibling(d.path(), 100);
    let requests = [
        LookupRequest {
            namespace: 0,
            id: 8,
            revision: Some(1),
        },
        LookupRequest {
            namespace: 0,
            id: 100,
            revision: Some(1),
        },
    ];
    assert!(matches!(
        r.lookup_many(&requests, HistoryReadMode::Batch),
        Err(canonical_core::Error::Corruption { .. })
    ));
    assert_eq!(r.storage_stats().point_reads, 2);
}
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static COST:Cell<(usize,usize)>=const{Cell::new((0,0))};}
struct Counting;
fn track(n: usize) {
    TRACK.with(|t| {
        if t.get() {
            COST.with(|c| {
                let (a, b) = c.get();
                c.set((a + 1, b + n));
            });
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        track(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        track(n);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn allocations<T>(f: impl FnOnce() -> T) -> (T, (usize, usize)) {
    COST.with(|c| c.set((0, 0)));
    TRACK.with(|t| t.set(true));
    let v = f();
    TRACK.with(|t| t.set(false));
    (v, COST.with(Cell::get))
}
#[test]
fn selected_member_allocations_exclude_sibling_payloads_but_include_physical_frame_buffer() {
    let (_d, mut r) = fixture();
    let req = LookupRequest {
        namespace: 0,
        id: 8,
        revision: Some(1),
    };
    let (full, fc) = allocations(|| {
        r.lookup_many(&[req], HistoryReadMode::ReferenceFull)
            .unwrap()
    });
    let (selected, sc) = allocations(|| r.lookup_many(&[req], HistoryReadMode::Selective).unwrap());
    assert_eq!(full.results[0].revision, selected.results[0].revision);
    assert!(fc.0 > sc.0 + 8, "full allocates each sibling payload");
    assert!(fc.1 >= sc.1 + 16 * 4096, "full materializes every payload");
    let (_d2, mut large) = fixture_n(1024);
    let (bigger, bc) = allocations(|| {
        large
            .lookup_many(&[req], HistoryReadMode::Selective)
            .unwrap()
    });
    assert_eq!(bigger.metrics.members_visited, 1024);
    assert_eq!(bigger.metrics.members_decoded, 1);
    assert_eq!(
        sc.0, bc.0,
        "allocation count must not grow with omitted member count"
    );
    assert_eq!(
        sc.1 - selected.metrics.physical_bytes_read as usize,
        bc.1 - bigger.metrics.physical_bytes_read as usize,
        "only the required full-frame scratch grows with omitted bytes"
    );
}
#[test]
fn selected_first_middle_last_work_for_one_two_and_maximum_member_counts() {
    for n in [1, 2, 1024] {
        let (_d, mut r) = fixture_n(n);
        for id in [1, n.div_ceil(2), n] {
            let req = LookupRequest {
                namespace: 0,
                id,
                revision: Some(1),
            };
            let expected = r.revision(0, id, 1).unwrap().clone();
            for mode in [
                HistoryReadMode::ReferenceFull,
                HistoryReadMode::Selective,
                HistoryReadMode::Batch,
            ] {
                let out = r.lookup_many(&[req], mode).unwrap();
                assert_eq!(out.results[0].revision, Some(expected.clone()));
                assert_eq!(out.metrics.members_visited, n);
                assert_eq!(
                    out.metrics.members_decoded,
                    if mode == HistoryReadMode::ReferenceFull {
                        n
                    } else {
                        1
                    }
                );
            }
        }
    }
}
#[test]
fn batch_repeats_physical_proof_on_next_call_after_a_crc_correct_sibling_change() {
    let (d, mut r) = fixture();
    let req = LookupRequest {
        namespace: 0,
        id: 8,
        revision: Some(1),
    };
    r.lookup_many(&[req, req], HistoryReadMode::Batch).unwrap();
    tamper_sibling(d.path(), 16);
    assert!(matches!(
        r.lookup_many(&[req], HistoryReadMode::Batch),
        Err(canonical_core::Error::Corruption { .. })
    ));
    assert_eq!(r.storage_stats().point_reads, 2);
}
