//! E13: only open/project/query/drop. No writes or corpus synthesis.
use crate::common::{
    self, Backend, Config, OpenBackend, ProjectionResult, ProjectionShape, Result,
};
use crate::{oracle, process};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
#[derive(Serialize)]
struct MemoryPhase {
    name: &'static str,
    cpu_ms: f64,
    working_set_bytes: u64,
    peak_working_set_bytes: u64,
}
fn phase(name: &'static str) -> Result<MemoryPhase> {
    let (cpu_ms, working_set_bytes, peak_working_set_bytes) = process::process_metrics()?;
    Ok(MemoryPhase {
        name,
        cpu_ms,
        working_set_bytes,
        peak_working_set_bytes,
    })
}
fn expected(cfg: Config, a: u64, o: u64, shape: ProjectionShape) -> ProjectionResult {
    common::project_facts(
        common::matches(cfg, a, o)
            .into_iter()
            .map(|id| common::fact(cfg, id, cfg.latest(id)))
            .collect(),
        shape,
    )
}
fn select(
    db: &mut dyn Backend,
    a: u64,
    o: u64,
    shape: ProjectionShape,
    variant: &str,
) -> Result<ProjectionResult> {
    match variant {
        "optimized" => db.select(a, o, shape),
        "materialized" => Ok(common::project_facts(db.filter_facts(a, o)?, shape)),
        "record_materialized_ids" if shape == ProjectionShape::Ids => {
            Ok(ProjectionResult::Ids(db.filter_record_identities(a, o)?))
        }
        _ => Err("variant materialized|record_materialized_ids|optimized required".into()),
    }
}
fn select_keys(db: &mut dyn Backend, a: u64, o: u64, optimized: bool) -> Result<Vec<u64>> {
    if !optimized {
        return db.filter(a, o);
    }
    match db.select(a, o, ProjectionShape::Ids)? {
        ProjectionResult::Ids(rows) => Ok(rows.into_iter().map(|row| row.id).collect()),
        _ => Err("optimized keys requires actual Ids output".into()),
    }
}
fn check(actual: &ProjectionResult, wanted: &ProjectionResult) -> Result<()> {
    if actual != wanted {
        return Err("projection result differs from independent oracle".into());
    }
    Ok(())
}
fn verify(db: &mut dyn Backend, cfg: Config) -> Result<serde_json::Value> {
    let mut all = oracle::Verifier::new(cfg);
    db.scan(&mut |f| all.observe(f))?;
    let history = all.finish()?;
    let mut heads = oracle::HeadVerifier::new(cfg);
    db.scan_heads(&mut |f| heads.observe(f))?;
    let heads = heads.finish()?;
    let mut edges = oracle::EdgeVerifier::new(cfg);
    db.scan_edges(&mut |e| edges.observe(e))?;
    let edges = edges.finish()?;
    for id in 1..=cfg.graph_nodes() {
        for incoming in [false, true] {
            if db.walk(id, 1, incoming)? != common::walk(cfg, id, 1, incoming) {
                return Err("full adjacency differs".into());
            }
        }
    }
    Ok(
        serde_json::json!({"history":history,"heads":heads,"edges":edges,"full_adjacency_verified":true}),
    )
}
#[derive(Serialize)]
struct Sample {
    sample: usize,
    action: u64,
    objective: u64,
    materialized_us: f64,
    optimized_us: f64,
    ratio_materialized_over_optimized: f64,
}
#[derive(Serialize)]
struct Distribution {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    total_ms: f64,
}
fn distribution(mut values: Vec<f64>) -> Distribution {
    values.sort_by(f64::total_cmp);
    let q = |p: f64| values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)];
    Distribution {
        samples: values.len(),
        p50_us: q(0.5),
        p95_us: q(0.95),
        p99_us: q(0.99),
        total_ms: values.iter().sum::<f64>() / 1000.0,
    }
}
pub fn run() -> Result<()> {
    // backend db rows revisions samples mode shape variant rotation [full-fact|record-ids]
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if !(9..=10).contains(&args.len()) {
        return Err("usage: backend db rows revisions samples latency|memory|verify ids|fields|full materialized|record_materialized_ids|optimized rotation [full-fact|record-ids]".into());
    }
    if cfg!(feature = "memory-profile") || canonical_memory::enabled() {
        return Err("System projection probe must be built without memory-profile".into());
    }
    let backend = &args[0];
    let path = Path::new(&args[1]);
    let cfg = Config {
        rows: args[2].parse()?,
        revisions: args[3].parse()?,
        samples: args[4].parse()?,
    };
    cfg.validate()?;
    let mode = args[5].as_str();
    let shape = match args[6].as_str() {
        "ids" => ProjectionShape::Ids,
        "fields" => ProjectionShape::Fields,
        "full" => ProjectionShape::Full,
        _ => return Err("shape ids|fields|full required".into()),
    };
    let variant = args[7].as_str();
    let rotation = args[8].parse::<usize>()?;
    let baseline = args.get(9).map(String::as_str).unwrap_or("full-fact");
    let materialized_variant = match baseline {
        "full-fact" => "materialized",
        "record-ids" if backend == "engine" && shape == ProjectionShape::Ids => {
            "record_materialized_ids"
        }
        _ => {
            return Err(
                "baseline full-fact or engine-only record-ids with shape ids required".into(),
            );
        }
    };
    if variant == "record_materialized_ids" && baseline != "record-ids" {
        return Err("record variant requires explicit record-ids baseline".into());
    }
    if mode == "memory" && variant != materialized_variant && variant != "optimized" {
        return Err("memory variant does not match declared baseline".into());
    }

    if mode == "latency_keys"
        && (backend != "engine" || shape != ProjectionShape::Ids || baseline != "record-ids")
    {
        return Err("latency_keys requires engine ids record-ids supplement".into());
    }
    let factory: OpenBackend = match backend.as_str() {
        "engine" => crate::engine::open,
        "sqlite" => crate::sqlite::open,
        "messagepack" => crate::codec_log::open_messagepack,
        "protobuf" => crate::codec_log::open_protobuf,
        _ => return Err("unknown backend".into()),
    };
    if !path.is_dir() {
        return Err("closed corpus directory missing".into());
    }
    let mut memory = vec![phase("before_open")?];
    let start = Instant::now();
    let mut db = factory(path, false, cfg)?;
    let reopen_ms = start.elapsed().as_secs_f64() * 1000.0;
    memory.push(phase("after_open")?);
    let start = Instant::now();
    db.project()?;
    let project_ms = start.elapsed().as_secs_f64() * 1000.0;
    memory.push(phase("after_project")?);
    let mut digest = Sha256::new();
    let mut samples = Vec::with_capacity(cfg.samples);
    let mut verification = None;
    let mut output_count = None;
    match mode {
        "verify" => {
            verification = Some(verify(db.as_mut(), cfg)?);
        }
        "latency" => {
            for v in [materialized_variant, "optimized"] {
                let r = select(db.as_mut(), 2, 2, shape, v)?;
                check(&r, &expected(cfg, 2, 2, shape))?;
            }
            // No JSON, hashing, oracle construction, or output drop in timed function calls.
            for sample in 0..cfg.samples {
                let a = 1 + sample as u64 % 7;
                let o = 1 + (sample as u64 * 5) % 17;
                let wanted = expected(cfg, a, o, shape);
                let mut times = [0.0; 2];
                let order = if (sample + rotation).is_multiple_of(2) {
                    [0, 1]
                } else {
                    [1, 0]
                };
                for i in order {
                    let v = if i == 0 {
                        materialized_variant
                    } else {
                        "optimized"
                    };
                    let start = Instant::now();
                    let out = std::hint::black_box(select(db.as_mut(), a, o, shape, v)?);
                    times[i] = start.elapsed().as_secs_f64() * 1e6;
                    check(&out, &wanted)?;
                    if i == 1 {
                        digest.update(serde_json::to_vec(&out)?);
                    }
                    drop(out);
                }
                samples.push(Sample {
                    sample,
                    action: a,
                    objective: o,
                    materialized_us: times[0],
                    optimized_us: times[1],
                    ratio_materialized_over_optimized: times[0] / times[1],
                });
            }
            for v in [materialized_variant, "optimized"] {
                let out = select(db.as_mut(), 999, 999, shape, v)?;
                check(&out, &expected(cfg, 999, 999, shape))?;
            }
        }
        "latency_keys" => {
            for optimized in [false, true] {
                if select_keys(db.as_mut(), 2, 2, optimized)? != common::matches(cfg, 2, 2) {
                    return Err("keys warmup differs from oracle".into());
                }
            }
            for sample in 0..cfg.samples {
                let a = 1 + sample as u64 % 7;
                let o = 1 + (sample as u64 * 5) % 17;
                let wanted = common::matches(cfg, a, o);
                let mut times = [0.0; 2];
                let order = if (sample + rotation).is_multiple_of(2) {
                    [0, 1]
                } else {
                    [1, 0]
                };
                for i in order {
                    let start = Instant::now();
                    let out = std::hint::black_box(select_keys(db.as_mut(), a, o, i == 1)?);
                    times[i] = start.elapsed().as_secs_f64() * 1e6;
                    if out != wanted {
                        return Err("keys result differs from independent oracle".into());
                    }
                    if i == 1 {
                        digest.update(serde_json::to_vec(&out)?);
                    }
                    drop(out);
                }
                samples.push(Sample {
                    sample,
                    action: a,
                    objective: o,
                    materialized_us: times[0],
                    optimized_us: times[1],
                    ratio_materialized_over_optimized: times[0] / times[1],
                });
            }
            for optimized in [false, true] {
                if !select_keys(db.as_mut(), 999, 999, optimized)?.is_empty() {
                    return Err("empty keys differs".into());
                }
            }
        }
        "memory" => {
            // One result batch (~rows/119) per isolated process; not 100k output rows.
            let out = select(db.as_mut(), 2, 2, shape, variant)?;
            memory.push(phase("output_live")?);
            check(&out, &expected(cfg, 2, 2, shape))?;
            output_count = Some(match &out {
                ProjectionResult::Ids(v) => v.len(),
                ProjectionResult::Fields(v) => v.len(),
                ProjectionResult::Full(v) => v.len(),
            });
            digest.update(serde_json::to_vec(&out)?);
            drop(out);
            memory.push(phase("output_drop")?);
        }
        _ => return Err("mode latency|latency_keys|memory|verify required".into()),
    }
    let details = db.details();
    drop(db);
    memory.push(phase("backend_drop")?);
    let materialized = if samples.is_empty() {
        None
    } else {
        Some(distribution(
            samples.iter().map(|s| s.materialized_us).collect(),
        ))
    };
    let optimized = if samples.is_empty() {
        None
    } else {
        Some(distribution(
            samples.iter().map(|s| s.optimized_us).collect(),
        ))
    };
    let report = serde_json::json!({"version":2,"baseline":baseline,"materialized_strategy":if mode=="latency_keys" {"legacy_keys"} else {materialized_variant},"optimized_strategy":if mode=="latency_keys" {"optimized_keys"}else{"select"},"result_kind":if mode=="latency_keys" {"u64_keys"}else{"typed_identity_or_rows"},"backend":backend,"config":cfg,"mode":mode,"shape":args[6],"variant":variant,"allocator":"System","hooks_compiled":false,"reopen_ms":reopen_ms,"project_ms":project_ms,"readiness_ms":reopen_ms+project_ms,"phases":memory,"samples":samples,"materialized":materialized,"optimized":optimized,"output_count":output_count,"result_digest":oracle::hex(&digest.finalize()),"verification":verification,"details":details,"notes":["Equivalent contents; no equivalence of all authorization/failure guarantees","Optimized means existing backend indexes/RAM/prepared statements; readiness includes project","Codec fields/full decode actual containing frame, then select member; no column decoder","System WS/Peak are process metrics, not exclusive component bytes; no Rust tracker used","Memory result batch is filter(2,2), not 100k outputs; live phase precedes oracle/serialization","Latency samples are warm OS cache; expectation/hash/drop outside timed call; any intermediate materialized output drop remains inside the baseline timed call","namespace=1 and schema namespace/id=(1,1); schema_version distinguishes absent extra v1 from null v2","Verification mode memory and CPU are not query memory metrics","latency_keys separately compares existing Backend.filter Records-to-u64 with select Ids-to-u64; both return only Vec<u64>, unlike full Identity metadata","full-fact baseline decodes Fact before converting; record-ids baseline materializes Query Records and maps metadata without decoding payload","record-ids supplement measures legacy-shaped path in current E13 sources, not a historical E11 binary speedup; cohorts/source hashes remain separate"]});
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validator_rejects_wrong_output() {
        assert!(
            check(
                &ProjectionResult::Ids(vec![]),
                &expected(
                    Config {
                        rows: 40,
                        revisions: 11,
                        samples: 1
                    },
                    2,
                    2,
                    ProjectionShape::Ids
                )
            )
            .is_err()
        );
    }
}
