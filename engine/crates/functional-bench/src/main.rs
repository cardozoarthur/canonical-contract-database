//! Functional equivalence benchmark; no Foundry adapter or dependency.
mod codec_log;
// Shared public harness contract also consumed by the projection probe.
pub mod common;
mod engine;
mod oracle;
mod process;
#[cfg(test)]
mod projection_tests;
mod sqlite;
use common::*;
use oracle::{EdgeVerifier, HeadVerifier, Verifier, hex};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const BATCH: usize = 256;
#[derive(Debug, Serialize)]
struct Distribution {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    total_ms: f64,
}
fn distribution(samples: &[Duration]) -> Distribution {
    let mut sorted: Vec<_> = samples.iter().map(Duration::as_secs_f64).collect();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        if sorted.is_empty() {
            0.0
        } else {
            sorted[((sorted.len() as f64 * p).ceil() as usize).saturating_sub(1)] * 1e6
        }
    };
    Distribution {
        samples: samples.len(),
        p50_us: percentile(0.5),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
        total_ms: sorted.iter().sum::<f64>() * 1000.0,
    }
}
fn measured<T>(
    times: &mut BTreeMap<String, Vec<Duration>>,
    label: &str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let t = Instant::now();
    let value = f()?;
    times.entry(label.into()).or_default().push(t.elapsed());
    Ok(value)
}
fn disk(path: &Path) -> Result<BTreeMap<String, u64>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, u64>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if kind.is_file() {
                out.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    entry.metadata()?.len(),
                );
            } else {
                return Err("unexpected nonregular benchmark file".into());
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(path, path, &mut files)?;
    Ok(files)
}
#[derive(Serialize)]
struct Report {
    backend: String,
    config: Config,
    batch_records: usize,
    verified_facts: u64,
    verified_edges: u64,
    history_digest: String,
    head_digest: String,
    verified_heads: u64,
    distinct_filter_pairs: usize,
    edge_digest: String,
    query_digest: String,
    base_first_revision_disk_bytes: u64,
    base_first_revision_files: BTreeMap<String, u64>,
    total_disk_bytes: u64,
    files: BTreeMap<String, u64>,
    timings: BTreeMap<String, Distribution>,
    write_records_per_second: f64,
    migration_records_per_second: f64,
    cpu_ms: f64,
    workload_cpu_ms: f64,
    verification_cpu_ms: f64,
    working_set_bytes: u64,
    peak_working_set_bytes: u64,
    phase_memory_bytes: BTreeMap<String, (u64, u64)>,
    details: BTreeMap<String, serde_json::Value>,
    notes: Vec<String>,
}
fn run(backend: &str, path: &Path, cfg: Config) -> Result<Report> {
    cfg.validate()?;
    let open: OpenBackend = match backend {
        "engine" => engine::open,
        "sqlite" => sqlite::open,
        "messagepack" => codec_log::open_messagepack,
        "protobuf" => codec_log::open_protobuf,
        _ => return Err("backend engine/sqlite/messagepack/protobuf required".into()),
    };
    if path.exists() {
        return Err("benchmark directory must be new".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(path)?;
    let (cpu_start, _, _) = process::process_metrics()?;
    let mut times = BTreeMap::new();
    let mut memories = BTreeMap::new();
    let mut db = measured(&mut times, "initial_open", || open(path, true, cfg))?;
    measured(&mut times, "register_schema_v1", || db.register_schema(1))?;
    let mut base_files = BTreeMap::new();
    for revision in 1..=cfg.revisions {
        for first in (1..=cfg.rows).step_by(BATCH) {
            let facts: Vec<_> = (first..=(first + BATCH as u64 - 1).min(cfg.rows))
                .map(|id| fact(cfg, id, revision))
                .collect();
            measured(&mut times, "write_fact_batch", || db.write_facts(&facts))?;
        }
        if revision == 1 {
            measured(&mut times, "base_index_flush", || db.flush())?;
            base_files = disk(path)?;
        }
    }
    for first in (1..cfg.graph_nodes()).step_by(BATCH) {
        let edges: Vec<_> = (first..=(first + BATCH as u64 - 1).min(cfg.graph_nodes() - 1))
            .map(edge)
            .collect();
        measured(&mut times, "write_edge_batch", || db.write_edges(&edges))?;
    }
    measured(&mut times, "register_schema_v2", || db.register_schema(2))?;
    if !measured(&mut times, "reject_bound_endpoint_migration", || {
        db.rejects_linked_schema_change(cfg)
    })? {
        return Err("backend weakened linked endpoint schema invariant".into());
    }
    for first in ((cfg.graph_nodes() + 1)..=cfg.rows).step_by(BATCH) {
        let facts: Vec<_> = (first..=(first + BATCH as u64 - 1).min(cfg.rows))
            .map(|id| fact(cfg, id, cfg.revisions + 1))
            .collect();
        measured(&mut times, "migrate_schema_batch", || {
            db.write_facts(&facts)
        })?;
    }
    measured(&mut times, "final_index_flush", || db.flush())?;
    let (_, mem, peak) = process::process_metrics()?;
    memories.insert("after_write".into(), (mem, peak));
    drop(db);
    let mut db = measured(&mut times, "recovery_open", || open(path, false, cfg))?;
    measured(&mut times, "build_query_and_graph_indices", || db.project())?;
    let (_, mem, peak) = process::process_metrics()?;
    memories.insert("after_projection".into(), (mem, peak));
    let mut digest = Sha256::new();
    for sample in 0..cfg.samples {
        let action = 1 + (sample as u64) % 7;
        let objective = 1 + (sample as u64 * 5) % 17;
        let ids = measured(&mut times, "filter_action_objective", || {
            db.filter(action, objective)
        })?;
        if ids != matches(cfg, action, objective) {
            return Err("filter differs from oracle".into());
        }
        digest.update(serde_json::to_vec(&ids)?);
        let start = 1 + (sample as u64 * 31) % (cfg.graph_nodes() - 3);
        for incoming in [false, true] {
            let node = if incoming { start + 3 } else { start };
            let mut edges = measured(
                &mut times,
                if incoming {
                    "walk_incoming_3"
                } else {
                    "walk_outgoing_3"
                },
                || db.walk(node, 3, incoming),
            )?;
            edges.sort();
            if edges != walk(cfg, node, 3, incoming) {
                return Err("walk differs from oracle".into());
            }
            digest.update(serde_json::to_vec(&edges)?);
        }
        let id = 1 + (sample as u64 * 7919) % cfg.rows;
        let last = cfg.latest(id) - 1;
        let first = last - 9;
        let history = measured(&mut times, "ten_previous_revisions", || {
            db.history(id, first, last)
        })?;
        let expected: Vec<_> = (first..=last).map(|rev| fact(cfg, id, rev)).collect();
        if history != expected {
            return Err("historical read differs from oracle".into());
        }
        digest.update(serde_json::to_vec(&history)?);
        let json = measured(&mut times, "json_on_demand", || db.json(id))?;
        let value: serde_json::Value = serde_json::from_slice(&json)?;
        if value != canonical_json(&fact(cfg, id, cfg.latest(id))) {
            return Err("JSON view differs from oracle".into());
        }
        digest.update(serde_json::to_vec(&value)?);
    }
    let (_, mem, peak) = process::process_metrics()?;
    memories.insert("after_queries".into(), (mem, peak));
    let (verification_start_cpu, _, _) = process::process_metrics()?;
    let mut verify = Verifier::new(cfg);
    measured(&mut times, "verify_all_fact_revisions", || {
        db.scan(&mut |f| verify.observe(f))
    })?;
    let history_digest = verify.finish()?;
    let mut verify = EdgeVerifier::new(cfg);
    measured(&mut times, "verify_all_edges", || {
        db.scan_edges(&mut |e| verify.observe(e))
    })?;
    let edge_digest = verify.finish()?;
    let mut heads = HeadVerifier::new(cfg);
    measured(&mut times, "verify_all_current_heads", || {
        db.scan_heads(&mut |f| heads.observe(f))
    })?;
    let head_digest = heads.finish()?;
    measured(&mut times, "verify_all_graph_adjacencies", || {
        for node in 1..=cfg.graph_nodes() {
            for incoming in [false, true] {
                let mut actual = db.walk(node, 1, incoming)?;
                actual.sort();
                if actual != walk(cfg, node, 1, incoming) {
                    return Err("complete adjacency differs from oracle".into());
                }
            }
        }
        Ok(())
    })?;
    let details = db.details();
    let files = disk(path)?;
    let (cpu_end, working_set_bytes, peak_working_set_bytes) = process::process_metrics()?;
    let timings: BTreeMap<_, _> = times
        .iter()
        .map(|(k, v)| (k.clone(), distribution(v)))
        .collect();
    let write_seconds = timings["write_fact_batch"].total_ms / 1000.0;
    let migration_seconds = timings["migrate_schema_batch"].total_ms / 1000.0;
    Ok(Report{backend:backend.into(),config:cfg,batch_records:BATCH,verified_facts:cfg.facts(),verified_edges:cfg.graph_nodes()-1,history_digest,head_digest,verified_heads:cfg.rows,distinct_filter_pairs:cfg.samples.min(119),edge_digest,query_digest:hex(&digest.finalize()),base_first_revision_disk_bytes:base_files.values().sum(),base_first_revision_files:base_files,total_disk_bytes:files.values().sum(),files,timings,write_records_per_second:(cfg.rows*cfg.revisions)as f64/write_seconds,migration_records_per_second:(cfg.rows-cfg.graph_nodes())as f64/migration_seconds,cpu_ms:cpu_end-cpu_start,workload_cpu_ms:verification_start_cpu-cpu_start,verification_cpu_ms:cpu_end-verification_start_cpu,working_set_bytes,peak_working_set_bytes,phase_memory_bytes:memories,details,notes:vec!["One process per run; warm OS cache; RAM/CPU include runtime and exact verification".into(),"Wire codecs use explicitly added persistence and indices; only workload equivalence, not equivalence of all failure/security guarantees".into(),"Schema migration covers unlinked half of identities; linked endpoint migration must be rejected; not an online whole-graph migration".into(),"No Foundry adapter, subscriptions, production or power-loss claims; no tokens measured".into()]})
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err(
            "usage: canonical-functional-bench BACKEND NEW_DIRECTORY ROWS REVISIONS SAMPLES".into(),
        );
    }
    let cfg = Config {
        rows: args[3].parse()?,
        revisions: args[4].parse()?,
        samples: args[5].parse()?,
    };
    let report = run(&args[1], &PathBuf::from(&args[2]), cfg)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn percentile_small_samples_and_boundaries_are_explicit() {
        let samples = [
            Duration::from_micros(1),
            Duration::from_micros(9),
            Duration::from_micros(4),
        ];
        let d = distribution(&samples);
        assert_eq!((d.p50_us, d.p95_us, d.p99_us), (4.0, 9.0, 9.0));
        assert_eq!(distribution(&[]).samples, 0);
    }
    #[test]
    fn oracle_covers_both_schema_versions_and_graph_boundaries() {
        let c = Config {
            rows: 8,
            revisions: 11,
            samples: 1,
        };
        assert!(c.validate().is_ok());
        assert_eq!(fact(c, 8, 12).schema_version, 2);
        assert_eq!(walk(c, 1, 3, false), vec![edge(1), edge(2), edge(3)]);
        assert_eq!(walk(c, 4, 3, true), vec![edge(1), edge(2), edge(3)]);
        assert!(walk(c, 8, 3, false).is_empty());
    }
}
