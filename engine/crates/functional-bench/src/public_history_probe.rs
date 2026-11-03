//! E20 public-history lifecycle probe. Cold windows never decode/hash public history.
use crate::{
    common::{self, Config, Result},
    process::process_metrics,
};
use canonical_registry::{
    IndexPolicy, Mutation, NewRecord, Registry, Retention, Revision, RevisionStatus, SchemaRef,
    Value,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
#[derive(Serialize)]
struct Phase {
    name: String,
    elapsed_ms: f64,
    cpu_ms: f64,
    working_set_before: u64,
    working_set_after: u64,
    process_peak_after: u64,
    identities: u64,
    revisions: u64,
    getter_p50_us: Option<f64>,
    getter_p99_us: Option<f64>,
}
#[derive(Serialize, Debug, PartialEq)]
struct Proof {
    name: String,
    identities: u64,
    revisions: u64,
    payload_bytes: u64,
    deleted: u64,
    minimum_sequence: u64,
    maximum_sequence: u64,
    digest: String,
}
#[derive(Serialize)]
struct Report {
    version: u32,
    accepted: bool,
    mode: String,
    config: Config,
    phases: Vec<Phase>,
    proofs: Vec<Proof>,
    notes: Vec<&'static str>,
}
struct Args {
    db: PathBuf,
    out: PathBuf,
    cfg: Config,
    mode: String,
}
fn parse(args: impl IntoIterator<Item = String>) -> Result<Args> {
    let mut db = None;
    let mut out = None;
    let mut rows = 100000;
    let mut revisions = 11;
    let mut mode = None;
    let mut args = args.into_iter();
    while let Some(key) = args.next() {
        let value = args.next().ok_or("missing argument value")?;
        match key.as_str() {
            "--db" => db = Some(PathBuf::from(value)),
            "--out" => out = Some(PathBuf::from(value)),
            "--rows" => rows = value.parse()?,
            "--revisions" => revisions = value.parse()?,
            "--mode" => mode = Some(value),
            _ => return Err("unknown argument".into()),
        }
    }
    let cfg = Config {
        rows,
        revisions,
        samples: 128,
    };
    cfg.validate()?;
    let mode = mode.ok_or("--mode one|all|mutation required")?;
    if !["one", "all", "mutation"].contains(&mode.as_str()) {
        return Err("invalid mode".into());
    }
    Ok(Args {
        db: db.ok_or("--db closed COPY required")?,
        out: out.ok_or("--out new JSON required")?,
        cfg,
        mode,
    })
}
fn phase<T>(name: &str, work: impl FnOnce() -> Result<T>) -> Result<(T, Phase)> {
    let (cpu, ws, _) = process_metrics()?;
    let start = Instant::now();
    let result = work()?;
    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.;
    let (end, after, peak) = process_metrics()?;
    Ok((
        result,
        Phase {
            name: name.into(),
            elapsed_ms,
            cpu_ms: end - cpu,
            working_set_before: ws,
            working_set_after: after,
            process_peak_after: peak,
            identities: 0,
            revisions: 0,
            getter_p50_us: None,
            getter_p99_us: None,
        },
    ))
}
fn ids(cfg: Config, one: bool) -> impl Iterator<Item = (u64, u64)> {
    let start = if one { cfg.rows } else { 1 };
    let edge_end = if one { 1 } else { cfg.graph_nodes() };
    (start..=cfg.rows)
        .map(|id| (1, id))
        .chain((1..edge_end).map(|id| (2, id)))
}
fn latest(cfg: Config, ns: u64, id: u64, updated: bool) -> u64 {
    if ns == 2 {
        1
    } else {
        cfg.latest(id) + u64::from(updated && id == cfg.rows)
    }
}
fn getters(
    r: &Registry,
    cfg: Config,
    name: &str,
    one: bool,
    updated: bool,
    retained: bool,
) -> Result<Phase> {
    let count = if one {
        1
    } else {
        cfg.rows + cfg.graph_nodes() - 1
    };
    let mut durations = Vec::with_capacity(count as usize); // Allocate harness storage before timing.
    let mut total = 0;
    let mut seen = 0;
    let (_, mut p) = phase(name, || {
        for (ns, id) in ids(cfg, one) {
            let expected = latest(cfg, ns, id, updated).min(if retained { 2 } else { u64::MAX });
            let start = Instant::now();
            let h = std::hint::black_box(r.history(ns, id));
            let elapsed = start.elapsed().as_nanos() as u64;
            if h.len() as u64 != expected {
                return Err("getter cardinality differs from workload".into());
            }
            durations.push(elapsed);
            seen += 1;
            total += h.len() as u64;
        }
        Ok(())
    })?;
    if seen != count {
        return Err("incomplete identity traversal".into());
    }
    durations.sort_unstable();
    let percentile = |pct: usize| {
        durations[(durations.len() * pct).div_ceil(100).saturating_sub(1)] as f64 / 1000.
    };
    p.identities = seen;
    p.revisions = total;
    p.getter_p50_us = Some(percentile(50));
    p.getter_p99_us = Some(percentile(99));
    drop(durations); // Working-set snapshots at window end include the common harness array.
    Ok(p)
}
fn expected_values(cfg: Config, id: u64, revision: u64) -> Vec<Option<Value>> {
    let f = common::fact(cfg, id, revision);
    let mut values = vec![
        Some(Value::UInt(f.action)),
        Some(Value::UInt(f.objective)),
        Some(Value::Utf8(f.text)),
    ];
    if f.schema_version == 2 {
        values.push(f.extra.map(Value::UInt));
    }
    values
}
fn verify_revision(r: &Registry, cfg: Config, ns: u64, id: u64, v: &Revision) -> Result<()> {
    let record = &v.record;
    if record.namespace != ns || record.id != id || v.deleted || record.revision == 0 {
        return Err("unexpected public identity/deleted/revision".into());
    }
    let expected_schema = SchemaRef {
        namespace: ns,
        id: 1,
        version: if ns == 2 || record.revision <= cfg.revisions {
            1
        } else {
            2
        },
    };
    if record.schema != expected_schema {
        return Err("public schema mismatch".into());
    }
    let values = r.decode_values(record.schema, &record.payload)?;
    let expected = if ns == 1 {
        expected_values(cfg, id, record.revision)
    } else {
        vec![Some(Value::Relation {
            source: canonical_registry::Reference { namespace: 1, id },
            kind: 1,
            target: canonical_registry::Reference {
                namespace: 1,
                id: id + 1,
            },
        })]
    };
    if values != expected {
        return Err("public payload differs from independent workload oracle".into());
    }
    Ok(())
}
fn proof(
    r: &Registry,
    cfg: Config,
    name: &str,
    one: bool,
    updated: bool,
    retained: bool,
    append_sequence: Option<u64>,
) -> Result<Proof> {
    let mut p = Proof {
        name: name.into(),
        identities: 0,
        revisions: 0,
        payload_bytes: 0,
        deleted: 0,
        minimum_sequence: u64::MAX,
        maximum_sequence: 0,
        digest: String::new(),
    };
    let mut digest = Sha256::new();
    for (ns, id) in ids(cfg, one) {
        let last = latest(cfg, ns, id, updated);
        let first = if retained {
            last.saturating_sub(1).max(1)
        } else {
            1
        };
        let h = r.history(ns, id);
        if h.len() as u64 != last - first + 1 {
            return Err("public proof cardinality mismatch".into());
        }
        p.identities += 1;
        let mut sequence = 0;
        for (rev, v) in (first..=last).zip(h) {
            if v.record.revision != rev || v.commit_sequence <= sequence {
                return Err("public revision order/sequence mismatch".into());
            }
            sequence = v.commit_sequence;
            if updated
                && ns == 1
                && id == cfg.rows
                && rev == last
                && Some(sequence) != append_sequence
            {
                return Err("appended revision sequence differs from receipt".into());
            }
            verify_revision(r, cfg, ns, id, v)?;
            if r.revision_status(ns, id, rev) != RevisionStatus::Available
                || r.revision(ns, id, rev) != Some(v)
            {
                return Err("public revision/status differs".into());
            }
            for n in [
                ns,
                id,
                rev,
                v.record.schema.namespace,
                v.record.schema.id,
                u64::from(v.record.schema.version),
                sequence,
                v.record.payload.len() as u64,
            ] {
                digest.update(n.to_le_bytes());
            }
            digest.update([u8::from(v.deleted)]);
            digest.update(&v.record.payload);
            p.revisions += 1;
            p.payload_bytes += v.record.payload.len() as u64;
            p.deleted += u64::from(v.deleted);
            p.minimum_sequence = p.minimum_sequence.min(sequence);
            p.maximum_sequence = p.maximum_sequence.max(sequence);
        }
        if r.revision_status(ns, id, 0) != RevisionStatus::Missing
            || r.revision_status(ns, id, last + 1) != RevisionStatus::Missing
        {
            return Err("missing revision status mismatch".into());
        }
        if first > 1
            && (r.revision_status(ns, id, 1) != RevisionStatus::Pruned
                || r.revision(ns, id, 1).is_some())
        {
            return Err("pruned revision reappeared".into());
        }
    }
    p.digest = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(p)
}
fn execute(db: &Path, cfg: Config, mode: &str) -> Result<Report> {
    let (mut r, opened) = phase("open", || {
        Ok(Registry::open_with_index_policy(db, IndexPolicy::Delta)?)
    })?;
    let mut report = Report {
        version: 1,
        accepted: false,
        mode: mode.into(),
        config: cfg,
        phases: vec![opened],
        proofs: vec![],
        notes: vec![
            "Fresh process required per mode; caller provides isolated physical copy; mutation intentionally changes only that copy.",
            "Cold windows precede every history/revision/snapshot verifier. Full external source verification/hashes are separate driver processes.",
            "Getter timing includes Instant/black_box/identity loop and cardinality checks, excludes payload decoding, hashing and percentile sorting.",
            "System working set and lifetime process peak are real Windows counters; peak includes harness timing arrays and later verification, not exclusive lazy-cache heap.",
            "CPU GetProcessTimes is quantized; first getter may report zero CPU ticks. No live private census or fabricated allocator/RSS attribution.",
            "Registry-only full touch does not promise the peak of simultaneous Query/Graph/full public history. Expected identity ranges are verified here; extra identities are checked by the external full verifier.",
        ],
    };
    if mode != "mutation" {
        let one = mode == "one";
        report
            .phases
            .push(getters(&r, cfg, "history_first", one, false, false)?);
        report
            .phases
            .push(getters(&r, cfg, "history_repeat", one, false, false)?);
        let (checked, p) = phase("proof_after_capture", || {
            proof(&r, cfg, "read", one, false, false, None)
        })?;
        report.phases.push(p);
        report.proofs.push(checked);
    } else {
        if r.record(1, cfg.rows).map(|record| record.revision) != Some(cfg.latest(cfg.rows)) {
            return Err("mutation requires untouched corpus copy".into());
        }
        report
            .phases
            .push(getters(&r, cfg, "target_warm", true, false, false)?);
        let (checked, p) = phase("target_proof_before_append", || {
            proof(&r, cfg, "before_append", true, false, false, None)
        })?;
        report.proofs.push(checked);
        report.phases.push(p);
        let last = cfg.latest(cfg.rows);
        let input = NewRecord {
            namespace: 1,
            id: cfg.rows,
            schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 2,
            },
            values: expected_values(cfg, cfg.rows, last + 1),
        };
        let (receipt, p) = phase("append_update", || {
            Ok(r.commit(
                [0xe0; 16],
                &[Mutation::Update {
                    record: input,
                    expected_revision: last,
                }],
            )?)
        })?;
        report.phases.push(p);
        report.phases.push(getters(
            &r,
            cfg,
            "target_after_append_first",
            true,
            true,
            false,
        )?);
        report.phases.push(getters(
            &r,
            cfg,
            "target_after_append_repeat",
            true,
            true,
            false,
        )?);
        let (checked, p) = phase("target_proof_after_append", || {
            proof(
                &r,
                cfg,
                "after_append",
                true,
                true,
                false,
                Some(receipt.commit_sequence),
            )
        })?;
        report.proofs.push(checked);
        report.phases.push(p);
        let (_, p) = phase("compact_keep_last_2", || {
            Ok(r.compact(Retention::KeepLast(2))?)
        })?;
        report.phases.push(p);
        report.phases.push(getters(
            &r,
            cfg,
            "post_compact_all_first",
            false,
            true,
            true,
        )?);
        report.phases.push(getters(
            &r,
            cfg,
            "post_compact_all_repeat",
            false,
            true,
            true,
        )?);
        let (checked, p) = phase("post_compact_proof", || {
            proof(
                &r,
                cfg,
                "compacted",
                false,
                true,
                true,
                Some(receipt.commit_sequence),
            )
        })?;
        report.proofs.push(checked);
        report.phases.push(p);
        let (_, p) = phase("drop_before_reopen", || {
            drop(r);
            Ok(())
        })?;
        report.phases.push(p);
        let (reopened, p) = phase("reopen_after_compact", || {
            Ok(Registry::open_with_index_policy(db, IndexPolicy::Delta)?)
        })?;
        r = reopened;
        report.phases.push(p);
        let (checked, p) = phase("reopen_proof", || {
            proof(
                &r,
                cfg,
                "reopened",
                false,
                true,
                true,
                Some(receipt.commit_sequence),
            )
        })?;
        if checked.digest != report.proofs.last().unwrap().digest {
            return Err("reopened public history differs".into());
        }
        report.proofs.push(checked);
        report.phases.push(p);
    }
    let (_, p) = phase("registry_drop", || {
        drop(r);
        Ok(())
    })?;
    report.phases.push(p);
    report.accepted = true;
    Ok(report)
}
pub fn run() -> Result<()> {
    let args = parse(std::env::args().skip(1))?;
    if args.out.exists() {
        return Err("output must not exist".into());
    }
    let report = execute(&args.db, args.cfg, &args.mode)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.out)?;
    file.write_all(&serde_json::to_vec_pretty(&report)?)?;
    println!(
        "{}",
        serde_json::json!({"accepted":report.accepted,"mode":args.mode,"output":args.out})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use canonical_registry::{Field, FieldType, Level, Namespace, RelationRule};
    fn fixture() -> (tempfile::TempDir, Config) {
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            rows: 8,
            revisions: 11,
            samples: 128,
        };
        let mut r = Registry::open(d.path()).unwrap();
        for id in [1, 2] {
            r.register_namespace(Namespace {
                id,
                name: format!("n{id}"),
                level: Level::Local,
            })
            .unwrap();
        }
        for version in [1, 2] {
            let mut fields = vec![
                Field {
                    label: "action".into(),
                    kind: FieldType::UInt,
                    optional: false,
                },
                Field {
                    label: "objective".into(),
                    kind: FieldType::UInt,
                    optional: false,
                },
                Field {
                    label: "text".into(),
                    kind: FieldType::Utf8,
                    optional: false,
                },
            ];
            if version == 2 {
                fields.push(Field {
                    label: "extra".into(),
                    kind: FieldType::UInt,
                    optional: true,
                });
            }
            r.register_schema(canonical_registry::Schema {
                reference: SchemaRef {
                    namespace: 1,
                    id: 1,
                    version,
                },
                fields,
            })
            .unwrap();
        }
        let edge = SchemaRef {
            namespace: 2,
            id: 1,
            version: 1,
        };
        r.register_schema(canonical_registry::Schema {
            reference: edge,
            fields: vec![Field {
                label: "relation".into(),
                kind: FieldType::Relation,
                optional: false,
            }],
        })
        .unwrap();
        r.register_relation_rule(RelationRule {
            schema: edge,
            kind: 1,
            source_schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 1,
            },
            target_schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 1,
            },
        })
        .unwrap();
        for rev in 1..=cfg.revisions + 1 {
            let first = if rev > cfg.revisions {
                cfg.graph_nodes() + 1
            } else {
                1
            };
            let version = if rev > cfg.revisions { 2 } else { 1 };
            let rows = (first..=cfg.rows)
                .map(|id| NewRecord {
                    namespace: 1,
                    id,
                    schema: SchemaRef {
                        namespace: 1,
                        id: 1,
                        version,
                    },
                    values: expected_values(cfg, id, rev),
                })
                .collect::<Vec<_>>();
            if rev == 1 {
                r.create_batch([rev as u8; 16], &rows).unwrap();
            } else {
                r.commit(
                    [rev as u8; 16],
                    &rows
                        .into_iter()
                        .map(|record| Mutation::Update {
                            record,
                            expected_revision: rev - 1,
                        })
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            }
        }
        let edges = (1..cfg.graph_nodes())
            .map(|id| NewRecord {
                namespace: 2,
                id,
                schema: edge,
                values: vec![Some(Value::Relation {
                    source: canonical_registry::Reference { namespace: 1, id },
                    kind: 1,
                    target: canonical_registry::Reference {
                        namespace: 1,
                        id: id + 1,
                    },
                })],
            })
            .collect::<Vec<_>>();
        r.create_batch([0xdd; 16], &edges).unwrap();
        drop(r);
        (d, cfg)
    }
    #[test]
    fn three_modes_verify_exact_public_lifecycle_on_small_corpus() {
        for mode in ["one", "all", "mutation"] {
            let (d, cfg) = fixture();
            let report = execute(d.path(), cfg, mode).unwrap();
            assert!(report.accepted);
            let full = report.proofs.last().unwrap();
            assert_eq!(full.identities, if mode == "one" { 1 } else { 11 });
            assert_eq!(
                full.revisions,
                match mode {
                    "one" => 12,
                    "all" => 95,
                    _ => 19,
                }
            );
            assert!(report.phases.iter().any(|p| p.name == "registry_drop"));
            if mode == "mutation" {
                assert_eq!(report.proofs[2].digest, report.proofs[3].digest);
            }
        }
    }
    #[test]
    fn parser_rejects_incomplete_unknown_and_out_of_workload_inputs() {
        for args in [
            vec!["--mode"],
            vec!["--mode", "wrong", "--db", "x", "--out", "y"],
            vec!["--rows", "1", "--mode", "all", "--db", "x", "--out", "y"],
            vec!["--unknown", "x"],
        ] {
            assert!(parse(args.into_iter().map(str::to_owned)).is_err());
        }
    }
}
