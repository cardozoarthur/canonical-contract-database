//! E14 real primary-index save/load/stream-validation and Registry policy probe.
use crate::common::{self, Backend, Config, ProjectionResult, ProjectionShape, Result};
use crate::{engine, oracle, process};
use canonical_index::{CacheFormat, CacheValidation, PrimaryIndex};
use canonical_registry::IndexPolicy;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path, time::Instant};
#[derive(Serialize)]
pub(crate) struct Phase {
    name: &'static str,
    cpu_ms: f64,
    working_set_bytes: u64,
    peak_working_set_bytes: u64,
}
pub(crate) fn phase(name: &'static str) -> Result<Phase> {
    let (cpu_ms, working_set_bytes, peak_working_set_bytes) = process::process_metrics()?;
    Ok(Phase {
        name,
        cpu_ms,
        working_set_bytes,
        peak_working_set_bytes,
    })
}
#[derive(Serialize)]
pub(crate) struct Cost {
    pub(crate) wall_ms: f64,
    cpu_ms: f64,
}
pub(crate) fn measure<T>(f: impl FnOnce() -> Result<T>) -> Result<(T, Cost)> {
    let (cpu, _, _) = process::process_metrics()?;
    let start = Instant::now();
    let value = f()?;
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (after, _, _) = process::process_metrics()?;
    Ok((
        value,
        Cost {
            wall_ms,
            cpu_ms: after - cpu,
        },
    ))
}
fn watermark(source: &Path) -> Result<u64> {
    let mut bytes = [0; 32];
    File::open(source.join("indexes/primary.cbi"))?.read_exact(&mut bytes)?;
    if &bytes[..8] != b"CBRIDX01" || ![1, 2].contains(&u16::from_le_bytes(bytes[8..10].try_into()?))
    {
        return Err("original legacy source cache required".into());
    }
    Ok(u64::from_le_bytes(bytes[16..24].try_into()?))
}
fn fingerprint(index: &PrimaryIndex) -> String {
    let mut h = Sha256::new();
    for entry in index.entries() {
        for v in [
            entry.key.namespace,
            entry.key.id,
            entry.key.revision,
            entry.location.commit_sequence,
            entry.location.frame_offset,
        ] {
            h.update(v.to_le_bytes());
        }
        h.update(entry.location.member.to_le_bytes());
        h.update([u8::from(entry.location.deleted)]);
    }
    oracle::hex(&h.finalize())
}
fn policy(name: &str) -> Result<IndexPolicy> {
    match name {
        "current" => Ok(IndexPolicy::Full),
        "grouped" => Ok(IndexPolicy::Grouped),
        "delta_varint" => Ok(IndexPolicy::Delta),
        "replay_only" => Ok(IndexPolicy::ReplayOnly),
        _ => Err("unknown index policy".into()),
    }
}
fn format(name: &str) -> Result<CacheFormat> {
    match name {
        "current" => Ok(CacheFormat::Legacy),
        "grouped" => Ok(CacheFormat::Grouped),
        "delta_varint" => Ok(CacheFormat::Delta),
        _ => Err("replay_only has no cache codec".into()),
    }
}
pub(crate) fn verify(db: &mut dyn Backend, cfg: Config) -> Result<serde_json::Value> {
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
pub(crate) struct QuerySample {
    sample: usize,
    operation: &'static str,
    elapsed_us: f64,
}
fn timed<T>(
    samples: &mut Vec<QuerySample>,
    sample: usize,
    name: &'static str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let start = Instant::now();
    let out = std::hint::black_box(f()?);
    samples.push(QuerySample {
        sample,
        operation: name,
        elapsed_us: start.elapsed().as_secs_f64() * 1e6,
    });
    Ok(out)
}
pub(crate) fn queries(db: &mut dyn Backend, cfg: Config) -> Result<(Vec<QuerySample>, String)> {
    let mut times = Vec::with_capacity(cfg.samples * 8);
    let mut digest = Sha256::new();
    for sample in 0..cfg.samples {
        let a = 1 + sample as u64 % 7;
        let o = 1 + (sample as u64 * 5) % 17;
        let wanted_keys = common::matches(cfg, a, o);
        let keys = timed(&mut times, sample, "keys_u64", || {
            match db.select(a, o, ProjectionShape::Ids)? {
                ProjectionResult::Ids(rows) => {
                    Ok(rows.into_iter().map(|row| row.id).collect::<Vec<u64>>())
                }
                _ => Err("keys_u64 requires actual Ids selection".into()),
            }
        })?;
        if keys != wanted_keys {
            return Err("keys_u64 differs from independent oracle".into());
        }
        digest.update(serde_json::to_vec(&keys)?);
        let wanted = wanted_keys
            .into_iter()
            .map(|id| common::fact(cfg, id, cfg.latest(id)))
            .collect::<Vec<_>>();
        for (shape, name) in [
            (ProjectionShape::Ids, "identities"),
            (ProjectionShape::Fields, "fields"),
            (ProjectionShape::Full, "full_fact"),
        ] {
            let out = timed(&mut times, sample, name, || db.select(a, o, shape))?;
            if out != common::project_facts(wanted.clone(), shape) {
                return Err("query differs from oracle".into());
            }
            digest.update(serde_json::to_vec(&out)?);
        }
        let start = 1 + (sample as u64 * 31) % (cfg.graph_nodes() - 3);
        for incoming in [false, true] {
            let id = if incoming { start + 3 } else { start };
            let out = timed(
                &mut times,
                sample,
                if incoming {
                    "graph_incoming_3"
                } else {
                    "graph_outgoing_3"
                },
                || db.walk(id, 3, incoming),
            )?;
            if out != common::walk(cfg, id, 3, incoming) {
                return Err("graph query differs".into());
            }
            digest.update(serde_json::to_vec(&out)?);
        }
        let id = 1 + (sample as u64 * 7919) % cfg.rows;
        let last = cfg.latest(id) - 1;
        let first = last - 9;
        let out = timed(&mut times, sample, "history_10", || {
            db.history(id, first, last)
        })?;
        if out
            != (first..=last)
                .map(|rev| common::fact(cfg, id, rev))
                .collect::<Vec<_>>()
        {
            return Err("history differs".into());
        }
        digest.update(serde_json::to_vec(&out)?);
        let json = timed(&mut times, sample, "json", || db.json(id))?;
        let value: serde_json::Value = serde_json::from_slice(&json)?;
        if value != common::canonical_json(&common::fact(cfg, id, cfg.latest(id))) {
            return Err("JSON differs".into());
        }
        digest.update(serde_json::to_vec(&value)?);
    }
    Ok((times, oracle::hex(&digest.finalize())))
}
pub fn run() -> Result<()> {
    // mode policy database output-cache rows revisions samples
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 7 {
        return Err("usage: codec_save|codec_load|prepare|verify|open policy database output-cache rows revisions samples".into());
    }
    if cfg!(feature = "memory-profile") || canonical_memory::enabled() {
        return Err("E14 System probe cannot include profiling hooks".into());
    }
    let mode = args[0].as_str();
    let name = args[1].as_str();
    let dbpath = Path::new(&args[2]);
    let artifact = Path::new(&args[3]);
    let cfg = Config {
        rows: args[4].parse()?,
        revisions: args[5].parse()?,
        samples: args[6].parse()?,
    };
    cfg.validate()?;
    let mut phases = vec![phase("before_setup")?];
    let mut metrics = serde_json::Map::new();
    let mut proof = None;
    let mut details = None;
    let mut digest = None;
    let mut samples = Vec::new();
    match mode {
        "codec_save" => {
            if artifact.exists() {
                return Err("codec output must be new".into());
            }
            let mark = watermark(dbpath)?;
            let expected = PrimaryIndex::load(&dbpath.join("indexes/primary.cbi"), mark)?;
            phases.push(phase("expected_index_live")?);
            let (_, cost) = measure(|| {
                expected.save_with_format(artifact, mark, format(name)?)?;
                Ok(())
            })?;
            metrics.insert(
                "save_total_encode_io_sync_replace".into(),
                serde_json::to_value(cost)?,
            );
            phases.push(phase("after_save")?);
            let (validated, cost) =
                measure(|| Ok(PrimaryIndex::validate_cache(artifact, mark, &expected)?))?;
            if validated != CacheValidation::Match {
                return Err("saved cache does not stream-match source".into());
            }
            metrics.insert("validate_stream".into(), serde_json::to_value(cost)?);
            phases.push(phase("after_stream_validation")?);
            metrics.insert("cache_bytes".into(), artifact.metadata()?.len().into());
            metrics.insert("entries".into(), expected.len().into());
            metrics.insert("watermark".into(), mark.into());
            digest = Some(fingerprint(&expected));
            drop(expected);
            phases.push(phase("after_drop_and_fingerprint")?);
        }
        "codec_load" => {
            let mark = watermark(dbpath)?;
            let (index, cost) = measure(|| Ok(PrimaryIndex::load(artifact, mark)?))?;
            metrics.insert("load_decode_construct".into(), serde_json::to_value(cost)?);
            phases.push(phase("decoded_index_live")?);
            metrics.insert("entries".into(), index.len().into());
            metrics.insert("cache_bytes".into(), artifact.metadata()?.len().into());
            digest = Some(fingerprint(&index));
            drop(index);
            phases.push(phase("after_drop_and_fingerprint")?);
        }
        "prepare" | "verify" | "open" => {
            let (mut db, cost) =
                measure(|| engine::open_with_policy(dbpath, false, cfg, policy(name)?))?;
            let open_ms = cost.wall_ms;
            metrics.insert("open".into(), serde_json::to_value(cost)?);
            phases.push(phase("after_open")?);
            if mode == "prepare" {
                let (_, cost) = measure(|| db.flush())?;
                metrics.insert("flush_selected_policy".into(), serde_json::to_value(cost)?);
                phases.push(phase("after_flush")?);
            } else {
                let (_, cost) = measure(|| db.project())?;
                metrics.insert("readiness_ms".into(), (open_ms + cost.wall_ms).into());
                metrics.insert("project".into(), serde_json::to_value(cost)?);
                phases.push(phase("after_project")?);
                if mode == "verify" {
                    proof = Some(verify(db.as_mut(), cfg)?);
                    phases.push(phase("after_verification")?);
                } else {
                    let (out, cost) = measure(|| queries(db.as_mut(), cfg))?;
                    samples = out.0;
                    digest = Some(out.1);
                    metrics.insert(
                        "queries_and_validation_harness".into(),
                        serde_json::to_value(cost)?,
                    );
                    phases.push(phase("after_queries")?);
                }
            }
            // details currently traverses history; keep its extra work outside measured phases above.
            details = Some(db.details());
            drop(db);
            phases.push(phase("after_drop_and_details")?);
        }
        _ => return Err("unknown probe mode".into()),
    }
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"version":1,"mode":mode,"policy":name,"config":cfg,"allocator":"System","hooks_compiled":false,"metrics":metrics,"phases":phases,"proof":proof,"details":details,"digest":digest,"query_samples":samples,"notes":["save_total includes actual encoder, I/O, flush, sync and replacement; no fabricated pure encode metric","System counters are process-level, not exclusive cache bytes; cache decode constructs same PrimaryIndex","codec_load is an independent process without expected index live; fingerprint entries Vec is built only after timed measurements/live phase","details traverses history; after_drop_and_details peak includes that extra work and is not the workload peak","queries_and_validation_harness includes oracle/hash/JSON validation; individual elapsed_us samples exclude these and exclude final output drop","State replay remains authoritative; stream checksum alone is insufficient without content match","warm OS cache; no cold device, power loss, production, Foundry or universal speedup claim"]})
        )?
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_policy_probe_compact_roundtrip() {
        use canonical_index::{IndexEntry, IndexKey, Location};
        let mut index = PrimaryIndex::new();
        for revision in 1..=2 {
            index
                .insert(IndexEntry {
                    key: IndexKey {
                        namespace: 1,
                        id: 1,
                        revision,
                    },
                    location: Location {
                        commit_sequence: revision,
                        frame_offset: revision * 256,
                        member: 0,
                        deleted: false,
                    },
                })
                .unwrap();
        }
        let root = std::env::temp_dir().join(format!(
            "e14-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        for format in [CacheFormat::Grouped, CacheFormat::Delta] {
            let file = root.join(format!("{format:?}.cbi"));
            index.save_with_format(&file, 2, format).unwrap();
            assert_eq!(
                PrimaryIndex::validate_cache(&file, 2, &index).unwrap(),
                CacheValidation::Match
            );
            let decoded = PrimaryIndex::load(&file, 2).unwrap();
            assert_eq!(fingerprint(&decoded), fingerprint(&index));
        }
    }
}
