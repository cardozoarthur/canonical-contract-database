//! E15 System macro comparison and independent native historical micro/profiler.
use crate::common::{Backend, Config, Result};
use crate::{engine, index_policy_probe as shared, oracle};
use canonical_registry::{HistoryReadMode, IndexPolicy, LookupRequest, Registry};
use canonical_relations::GraphLayout;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
fn options(name: &str) -> Result<engine::EngineOptions> {
    match name {
        "native_reference" => Ok(engine::EngineOptions {
            index_policy: IndexPolicy::Full,
            graph_layout: GraphLayout::Legacy,
            history_mode: HistoryReadMode::ReferenceFull,
        }),
        "native_optimized" => Ok(engine::EngineOptions {
            index_policy: IndexPolicy::Delta,
            graph_layout: GraphLayout::Compact,
            history_mode: HistoryReadMode::Batch,
        }),
        _ => Err("unknown native configuration".into()),
    }
}
fn backend(name: &str, path: &Path, cfg: Config) -> Result<Box<dyn Backend>> {
    match name {
        "native_reference" | "native_optimized" => {
            engine::open_with_options(path, false, cfg, options(name)?)
        }
        "sqlite" => crate::sqlite::open(path, false, cfg),
        "messagepack" => crate::codec_log::open_messagepack(path, false, cfg),
        "protobuf" => crate::codec_log::open_protobuf(path, false, cfg),
        _ => Err("unknown backend".into()),
    }
}
fn requests(cfg: Config, kind: &str, sample: usize) -> Vec<LookupRequest> {
    let r = |id, revision| LookupRequest {
        namespace: 1,
        id,
        revision,
    };
    let id = 1 + (sample as u64 * 7919) % cfg.rows;
    match kind {
        "H1" => vec![r(id, Some(1 + (sample as u64 % cfg.latest(id))))],
        "H10" => {
            let last = cfg.latest(id) - 1;
            (last - 9..=last).map(|rev| r(id, Some(rev))).collect()
        }
        "Hshared" => (1..=cfg.rows.min(128)).map(|id| r(id, Some(1))).collect(),
        "Hmixed" => vec![
            r(1, Some(1)),
            r(cfg.rows, Some(cfg.latest(cfg.rows))),
            r(1, Some(1)),
            r(cfg.rows + 1, Some(1)),
            r(0, None),
            r(1, Some(0)),
            r(1, None),
        ],
        _ => vec![],
    }
}
fn metrics(m: &canonical_registry::HistoryReadMetrics) -> serde_json::Value {
    json!({"physical_frame_reads":m.physical_frame_reads,"physical_bytes_read":m.physical_bytes_read,"header_crc_bytes_verified":m.header_crc_bytes_verified,"payload_crc_bytes_verified":m.payload_crc_bytes_verified,"prefix_payload_bytes_compared":m.prefix_payload_bytes_compared,"members_visited":m.members_visited,"members_decoded":m.members_decoded,"output_records":m.output_records,"output_payload_bytes":m.output_payload_bytes})
}
fn check_snapshot(s: &canonical_memory::Snapshot) -> Result<()> {
    if s.overflowed
        || s.peak_components.iter().sum::<u64>() != s.global_peak_requested
        || s.components
            .iter()
            .map(|x| x.current_requested)
            .sum::<u64>()
            != s.total_current_requested
    {
        return Err("memory ledger not reconciled".into());
    }
    Ok(())
}
fn allocation_delta(
    b: &canonical_memory::Snapshot,
    a: &canonical_memory::Snapshot,
) -> serde_json::Value {
    let allocated: u64 = a
        .components
        .iter()
        .zip(&b.components)
        .map(|(a, b)| a.total_allocated - b.total_allocated)
        .sum();
    let freed: u64 = a
        .components
        .iter()
        .zip(&b.components)
        .map(|(a, b)| a.total_freed - b.total_freed)
        .sum();
    let alloc_calls: u64 = a
        .components
        .iter()
        .zip(&b.components)
        .map(|(a, b)| a.alloc_calls - b.alloc_calls)
        .sum();
    let realloc_calls: u64 = a
        .components
        .iter()
        .zip(&b.components)
        .map(|(a, b)| a.realloc_calls - b.realloc_calls)
        .sum();
    json!({"allocated_requested_bytes":allocated,"freed_requested_bytes":freed,"alloc_calls":alloc_calls,"realloc_calls":realloc_calls,"output_live_requested_change":a.total_current_requested as i128-b.total_current_requested as i128,"window_peak_additional_requested":a.global_peak_requested.saturating_sub(b.total_current_requested),"header_padding_live_change":a.current_overhead as i128-b.current_overhead as i128})
}
fn micro(path: &Path, cfg: Config, profile: bool) -> Result<serde_json::Value> {
    let _harness = canonical_memory::scope(canonical_memory::Component::Harness);
    let mut phases = vec![shared::phase("before_open")?];
    let (mut registry, open) =
        shared::measure(|| Ok(Registry::open_with_index_policy(path, IndexPolicy::Delta)?))?;
    phases.push(shared::phase("after_open")?);
    let mut rows = Vec::with_capacity(cfg.samples * 15);
    let mut digest = Sha256::new();
    for sample in 0..cfg.samples {
        for kind in ["Hempty", "H1", "H10", "Hshared", "Hmixed"] {
            let req = requests(cfg, kind, sample);
            let expected = req
                .iter()
                .map(|q| {
                    registry
                        .history(q.namespace, q.id)
                        .iter()
                        .find(|rev| {
                            q.revision.map_or(
                                !rev.deleted
                                    && registry
                                        .record(q.namespace, q.id)
                                        .is_some_and(|r| r.revision == rev.record.revision),
                                |r| r == rev.record.revision,
                            )
                        })
                        .cloned()
                })
                .collect::<Vec<_>>();
            let mut h = Sha256::new();
            for q in &req {
                h.update(q.namespace.to_le_bytes());
                h.update(q.id.to_le_bytes());
                h.update([u8::from(q.revision.is_some())]);
                h.update(q.revision.unwrap_or(0).to_le_bytes());
            }
            let request_hash = oracle::hex(&h.finalize());
            let modes = [
                HistoryReadMode::ReferenceFull,
                HistoryReadMode::Selective,
                HistoryReadMode::Batch,
            ];
            for index in 0..3 {
                let mode = modes[(sample + index) % 3];
                if profile {
                    canonical_memory::reset_peak_window();
                }
                let before = profile.then(canonical_memory::snapshot);
                let (out, cost) = shared::measure(|| Ok(registry.lookup_many(&req, mode)?))?;
                let after = profile.then(canonical_memory::snapshot);
                if out
                    .results
                    .iter()
                    .map(|r| r.revision.clone())
                    .collect::<Vec<_>>()
                    != expected
                {
                    return Err(
                        "historical micro result differs from immutable registry history oracle"
                            .into(),
                    );
                }
                if let (Some(b), Some(a)) = (&before, &after) {
                    check_snapshot(b)?;
                    check_snapshot(a)?;
                }
                let mut result_hash = Sha256::new();
                for revision in &expected {
                    match revision {
                        None => result_hash.update([0]),
                        Some(rev) => {
                            result_hash.update([1, u8::from(rev.deleted)]);
                            let r = &rev.record;
                            for v in [
                                r.namespace,
                                r.id,
                                r.revision,
                                r.schema.namespace,
                                r.schema.id,
                                r.schema.version as u64,
                            ] {
                                result_hash.update(v.to_le_bytes());
                            }
                            result_hash.update((r.payload.len() as u64).to_le_bytes());
                            result_hash.update(&r.payload);
                        }
                    }
                }
                let result_hash = oracle::hex(&result_hash.finalize());
                digest.update(request_hash.as_bytes());
                digest.update(result_hash.as_bytes());
                rows.push(json!({"sample":sample,"kind":kind,"mode":format!("{mode:?}"),"requests":req.len(),"request_hash":request_hash,"result_hash":result_hash,"cost":cost,"metrics":metrics(&out.metrics),"allocation_delta":before.as_ref().zip(after.as_ref()).map(|(b,a)|allocation_delta(b,a)),"allocation_before":before,"allocation_after_output_live":after}));
            }
        }
    }
    phases.push(shared::phase("after_micro_workload")?);
    drop(registry);
    phases.push(shared::phase("after_registry_drop")?);
    Ok(
        json!({"open":open,"phases":phases,"samples":rows,"digest":oracle::hex(&digest.finalize()),"notes":["No cross-call application cache: Batch deduplicates physical reads only within one lookup_many call","Hshared requests IDs1..min(rows,128),revision1; inspect actual physical_frame_reads before assuming one shared frame","Hempty/Hmixed include empty, missing id0/out-of-range,revision0,current and duplicated real requests","Registry.history expected revisions cloned before allocation/timing window; complete selected Record bytes/identity/schema/tombstone compared outside it","physical_bytes_read uses actual public metric definition, not OS SSD device I/O; header CRC/payload CRC and replay-mirror prefix comparison are distinct","profile timing is instrumented diagnostic only, not System latency; snapshots preserve origin and output-live counters; do not sum independent component peaks"]}),
    )
}
pub fn run() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 6 {
        return Err("usage: prepare|verify|macro|micro|micro-profile backend directory rows revisions samples".into());
    }
    let mode = args[0].as_str();
    let name = args[1].as_str();
    let path = Path::new(&args[2]);
    let cfg = Config {
        rows: args[3].parse()?,
        revisions: args[4].parse()?,
        samples: args[5].parse()?,
    };
    cfg.validate()?;
    let tracked = canonical_memory::enabled();
    if mode == "micro-profile" {
        if !tracked || !cfg!(feature = "memory-profile") {
            return Err("micro-profile requires dedicated tracker binary/hooks".into());
        }
    } else if tracked || cfg!(feature = "memory-profile") {
        return Err("System timing requires no tracker/hooks".into());
    }
    if mode == "micro" || mode == "micro-profile" {
        if name != "native_optimized" {
            return Err("micro requires native_optimized Delta source".into());
        }
        let detail = micro(path, cfg, tracked)?;
        println!(
            "{}",
            serde_json::to_string(
                &json!({"version":1,"mode":mode,"backend":name,"config":cfg,"allocator":if tracked{"Tracker"}else{"System"},"hooks_compiled":cfg!(feature="memory-profile"),"micro":detail})
            )?
        );
        return Ok(());
    }
    let mut phases = vec![shared::phase("before_open")?];
    let (mut db, open) = shared::measure(|| backend(name, path, cfg))?;
    let open_ms = open.wall_ms;
    phases.push(shared::phase("after_open")?);
    let mut project = None;
    let mut readiness = None;
    let mut proof = None;
    let mut samples = None;
    let mut digest = None;
    let mut flush = None;
    let mut queries_harness = None;
    match mode {
        "prepare" => {
            if !name.starts_with("native_") {
                return Err("prepare is native copies only".into());
            }
            flush = Some(shared::measure(|| db.flush())?.1);
            phases.push(shared::phase("after_flush")?);
        }
        "verify" | "macro" => {
            let (_, cost) = shared::measure(|| db.project())?;
            readiness = Some(open_ms + cost.wall_ms);
            project = Some(cost);
            phases.push(shared::phase("after_project")?);
            if mode == "verify" {
                proof = Some(shared::verify(db.as_mut(), cfg)?);
                phases.push(shared::phase("after_verification")?);
            } else {
                let (out, cost) = shared::measure(|| shared::queries(db.as_mut(), cfg))?;
                queries_harness = Some(cost);
                samples = Some(out.0);
                digest = Some(out.1);
                phases.push(shared::phase("after_queries")?);
            }
        }
        _ => return Err("unknown e15 mode".into()),
    }
    let details = db.details();
    drop(db);
    phases.push(shared::phase("after_drop_and_details")?);
    println!(
        "{}",
        serde_json::to_string(
            &json!({"version":1,"mode":mode,"backend":name,"config":cfg,"allocator":"System","hooks_compiled":false,"open":open,"project":project,"readiness_ms":readiness,"flush_selected":flush,"queries_harness":queries_harness,"proof":proof,"query_samples":samples,"digest":digest,"phases":phases,"details":details,"notes":["Fresh same-source four backends; native_reference=Full+Legacy+ReferenceFull, native_optimized=Delta+Compact+Batch","SQLite/MessagePack/Protobuf use existing real optimized adapters, with persisted/index/full-frame codec strategy declared in details; not all failure/security guarantees equivalent","keys_u64,Identity,Fields,Fact,Edge graph3 both directions,history10 andJSON are different content shapes","Open/project readiness calculated per run; full verifier and details history traversal excluded from macro measured phases","After_queries process peak includes oracle/hash harness, individual elapsed_us samples exclude it; after_drop_and_details includes additional history traversal","No cross-call application cache; no persisted adjacency wire; no Foundry/universal performance guarantee"]})
        )?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consumer_empty_and_missing_modes() {
        let path = std::env::temp_dir().join(format!(
            "e15-consumer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut registry = Registry::open_with_index_policy(path, IndexPolicy::ReplayOnly).unwrap();
        for mode in [
            HistoryReadMode::ReferenceFull,
            HistoryReadMode::Selective,
            HistoryReadMode::Batch,
        ] {
            assert!(registry.lookup_many(&[], mode).unwrap().results.is_empty());
            let result = registry
                .lookup_many(
                    &[LookupRequest {
                        namespace: 0,
                        id: 0,
                        revision: Some(0),
                    }],
                    mode,
                )
                .unwrap();
            assert_eq!(result.results.len(), 1);
            assert!(result.results[0].revision.is_none());
            assert_eq!(result.metrics.physical_frame_reads, 0);
        }
    }
}
