use canonical_registry::*;
use std::fs;

fn sr() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn input(namespace: u64, id: u64, value: u64) -> NewRecord {
    NewRecord {
        namespace,
        id,
        schema: sr(),
        values: vec![Some(Value::UInt(value))],
    }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("ns{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    r.register_schema(Schema {
        reference: sr(),
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    r.create_batch(
        [1; 16],
        &[input(1, 1, 10), input(1, 2, 20), input(2, 1, 99)],
    )
    .unwrap();
    r.update(input(1, 1, 30), 1).unwrap();
    r.delete(1, 2, 1).unwrap();
    r
}

#[test]
fn physical_lookup_matches_authoritative_history_and_respects_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    for (ns, id) in [(1, 1), (1, 2), (2, 1)] {
        let history = r.history(ns, id).to_vec();
        for expected in history {
            let result = r.lookup(ns, id, Some(expected.record.revision)).unwrap();
            assert_eq!(result.revision, Some(expected));
            assert_eq!(result.metrics.point_reads, 1);
            assert_eq!(result.metrics.frames_scanned, 0);
            assert!(result.metrics.point_bytes > 0);
        }
    }
    let deleted = r.lookup(1, 2, None).unwrap();
    assert_eq!(deleted.revision, None);
    assert_eq!(deleted.metrics.point_reads, 0);
    assert_eq!(
        r.lookup(2, 1, None)
            .unwrap()
            .revision
            .unwrap()
            .record
            .payload,
        r.record(2, 1).unwrap().payload
    );
    assert_eq!(r.lookup(1, 999, None).unwrap().revision, None);
}

#[test]
fn absent_corrupt_or_stale_cache_rebuilds_without_changing_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.flush_index().unwrap();
    let index = dir.path().join("indexes/primary.cbi");
    let cached = fs::read(&index).unwrap();
    drop(r);
    let log = fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(r.index_status().loaded);
    assert!(r.lookup(1, 2, None).unwrap().revision.is_none());
    drop(r);
    fs::remove_file(&index).unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(!r.index_status().loaded);
    assert!(r.index_status().rebuilt);
    r.flush_index().unwrap();
    drop(r);
    let mut bad = fs::read(&index).unwrap();
    let n = bad.len();
    bad[n - 1] ^= 1;
    fs::write(&index, bad).unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(r.index_status().rebuilt);
    assert!(r.lookup(1, 2, None).unwrap().revision.is_none());
    r.update(input(1, 1, 40), 2).unwrap();
    drop(r);
    fs::write(&index, cached).unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(r.index_status().rebuilt);
    assert_eq!(
        r.lookup(1, 1, None)
            .unwrap()
            .revision
            .unwrap()
            .record
            .revision,
        3
    );
    let final_log = fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    assert!(final_log.starts_with(&log));
}

#[test]
fn lookup_remains_indexed_on_ten_thousand_identities_in_two_namespaces() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    for chunk in 0..10u64 {
        let records: Vec<_> = (0..1000)
            .map(|i| input(if i % 2 == 0 { 1 } else { 2 }, chunk * 1000 + i + 100, i))
            .collect();
        r.create_batch((chunk as u128 + 2).to_le_bytes(), &records)
            .unwrap();
    }
    r.update(input(1, 500, 501), 1).unwrap();
    r.flush_index().unwrap();
    drop(r);
    let mut r = Registry::open(dir.path()).unwrap();
    let before = r.storage_stats();
    assert!(before.replay_frames > 0); // cold start is explicitly not promised scan-free
    let result = r.lookup(1, 500, Some(2)).unwrap();
    let after = r.storage_stats();
    assert_eq!(
        result.revision.unwrap(),
        r.revision(1, 500, 2).unwrap().clone()
    );
    assert_eq!(after.replay_frames, before.replay_frames);
    assert_eq!(after.point_reads - before.point_reads, 1);
    assert_eq!(result.metrics.index_probes, 1);
    assert_eq!(result.metrics.frames_scanned, 0);
}

#[test]
fn cache_with_valid_checksum_but_wrong_member_is_rebuilt_from_log() {
    use canonical_index::{IndexEntry, PrimaryIndex};
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    let watermark = r.snapshot().commit_sequence;
    r.flush_index().unwrap();
    drop(r);
    let path = dir.path().join("indexes/primary.cbi");
    let cached = PrimaryIndex::load(&path, watermark).unwrap();
    let mut altered = PrimaryIndex::new();
    for entry in cached.entries() {
        let mut location = entry.location;
        if entry.key.namespace == 1 && entry.key.id == 1 && entry.key.revision == 1 {
            location.member = 2;
        }
        altered
            .insert(IndexEntry {
                key: entry.key,
                location,
            })
            .unwrap();
    }
    altered.save(&path, watermark).unwrap();
    let before = fs::read(dir.path().join("segments/00000001.cbr")).unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert_eq!(
        r.index_status().reason,
        Some(IndexRebuildReason::ContentMismatch)
    );
    assert_eq!(
        r.lookup(1, 1, Some(1)).unwrap().revision.unwrap(),
        r.revision(1, 1, 1).unwrap().clone()
    );
    assert_eq!(
        fs::read(dir.path().join("segments/00000001.cbr")).unwrap(),
        before
    );
}

#[test]
fn post_open_corruption_is_signaled_instead_of_returning_cached_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.flush_index().unwrap();
    let path = dir.path().join("segments/00000001.cbr");
    let mut bytes = fs::read(&path).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    fs::write(&path, bytes).unwrap();
    assert!(r.lookup(1, 2, Some(2)).is_err());
    assert!(r.create(1, 99, sr(), &[Some(Value::UInt(1))]).is_err());
}

#[test]
fn frozen_primary_cache_and_journal_load_together() {
    let dir = tempfile::tempdir().unwrap();
    for folder in ["segments", "indexes"] {
        fs::create_dir(dir.path().join(folder)).unwrap();
    }
    fs::write(
        dir.path().join("manifest.cbr"),
        include_bytes!("fixtures/indexed-v1/manifest.cbr"),
    )
    .unwrap();
    fs::write(
        dir.path().join("segments/00000001.cbr"),
        include_bytes!("fixtures/indexed-v1/segments/00000001.cbr"),
    )
    .unwrap();
    fs::write(
        dir.path().join("indexes/primary.cbi"),
        include_bytes!("fixtures/indexed-v1/indexes/primary.cbi"),
    )
    .unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    assert!(r.index_status().loaded);
    let result = r.lookup(1, 1, None).unwrap();
    assert_eq!(
        r.decode_values(sr(), &result.revision.unwrap().record.payload)
            .unwrap(),
        vec![Some(Value::UInt(871))]
    );
    assert_eq!(result.metrics.point_reads, 1);
    assert_eq!(result.metrics.frames_scanned, 0);
}
