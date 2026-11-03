//! Local release benchmark with equivalent indexed query and independent scan oracle.
use canonical_query::*;
use canonical_registry::*;
use std::{collections::BTreeSet, time::Instant};

fn percentile(sorted: &[u128], p: usize) -> u128 {
    sorted[((sorted.len() - 1) * p).div_ceil(100)]
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("usage: benchmark NEW_DB_DIR")?;
    let mut registry = Registry::open(directory)?;
    registry.register_namespace(Namespace {
        id: 1,
        name: "benchmark".into(),
        level: Level::Local,
    })?;
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    registry.register_schema(Schema {
        reference: sr,
        fields: vec![
            Field {
                label: "file_id".into(),
                kind: FieldType::UInt,
                optional: false,
            },
            Field {
                label: "timestamp_ms_utc".into(),
                kind: FieldType::UInt,
                optional: false,
            },
        ],
    })?;
    for chunk in 0..10u64 {
        let records: Vec<_> = (0..1000u64)
            .map(|i| {
                let id = chunk * 1000 + i + 1;
                NewRecord {
                    namespace: 1,
                    id,
                    schema: sr,
                    values: vec![Some(Value::UInt(id % 1000)), Some(Value::UInt(1000 + id))],
                }
            })
            .collect();
        registry.create_batch(
            (chunk + 1).to_le_bytes().repeat(2).try_into().unwrap(),
            &records,
        )?;
    }
    let snapshot = registry.snapshot();
    let start = Instant::now();
    let mut engine = QueryEngine::new(&registry, snapshot.clone())?;
    let build_us = start.elapsed().as_micros();
    let subject = Subject {
        id: "benchmark-reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let query = Query {
        namespaces: vec![1],
        schemas: vec![sr],
        filter: Predicate::Eq(
            FieldRef {
                schema: sr,
                position: 0,
            },
            Value::UInt(42),
        ),
        order: Order::default(),
        limit: 1000,
        cursor: None,
    };
    // Decode the scan oracle once too, so timed scan is not penalized by deserialization.
    let oracle: Vec<_> = snapshot
        .iter()
        .map(|record| {
            let values = registry
                .decode_values(record.schema, &record.payload)
                .unwrap();
            let Some(Value::UInt(file)) = values[0] else {
                unreachable!()
            };
            (record.clone(), file)
        })
        .collect();
    let expected: Vec<_> = oracle
        .iter()
        .filter(|(_, file)| *file == 42)
        .map(|(record, _)| record.clone())
        .collect();
    let mut indexed = Vec::new();
    let mut scan = Vec::new();
    let mut last_plan = None;
    for _ in 0..200 {
        let start = Instant::now();
        let page = engine.query(&subject, &query)?;
        indexed.push(start.elapsed().as_nanos());
        assert_eq!(page.records, expected);
        last_plan = Some(page.plan);
        let start = Instant::now();
        let found: Vec<_> = oracle
            .iter()
            .filter(|(_, file)| *file == 42)
            .map(|(record, _)| record.clone())
            .collect();
        scan.push(start.elapsed().as_nanos());
        assert_eq!(found, expected);
    }
    indexed.sort_unstable();
    scan.sort_unstable();
    println!(
        "records=10000 repetitions=200 fixture=file_id=id%1000 target42 expected_count={}",
        expected.len()
    );
    println!("build_secondary_indexes_us={build_us}");
    println!(
        "indexed_ns p50={} p95={} p99={}",
        percentile(&indexed, 50),
        percentile(&indexed, 95),
        percentile(&indexed, 99)
    );
    println!(
        "scan_ns p50={} p95={} p99={}",
        percentile(&scan, 50),
        percentile(&scan, 95),
        percentile(&scan, 99)
    );
    let plan = last_plan.unwrap();
    println!(
        "plan={:?} candidates={} rows_examined={} index_lookups={}",
        plan.strategy, plan.candidates, plan.rows_examined, plan.index_lookups
    );
    println!(
        "not_measured=RSS,CPU,total_storage,tokens,production,power_failure; snapshot_and_indexes_resident_in_RAM=true"
    );
    Ok(())
}
