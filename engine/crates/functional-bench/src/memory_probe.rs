//! Read-only E12 lifecycle probe. Serialization and verification are outside captured windows.
use crate::{
    common::{self, Config, Edge, Fact, Result},
    oracle::{EdgeVerifier, HeadVerifier, Verifier},
    process::process_metrics,
};
use canonical_memory::{ALL_COMPONENTS, Component, Snapshot};
use canonical_query::{FieldRef, Order, Predicate, Query, QueryEngine, Subject};
use canonical_registry::{Record, Reference, Registry, SchemaRef, Value};
use canonical_relations::{Direction, GraphLayout, GraphView, WalkOptions};
use canonical_storage::Store;
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};
#[derive(Serialize)]
struct Phase {
    name: &'static str,
    since_start_ms: f64,
    cpu_since_start_ms: f64,
    working_set_bytes: u64,
    process_peak_bytes: u64,
    allocation: Snapshot,
    census: BTreeMap<&'static str, u64>,
}
#[derive(Serialize)]
struct ProbeReport {
    version: u32,
    tracked: bool,
    hooks_compiled: bool,
    graph_layout: &'static str,
    config: Config,
    phases: Vec<Phase>,
    component_names: Vec<&'static str>,
    notes: Vec<&'static str>,
}
fn check(snapshot: &Snapshot) -> Result<()> {
    if snapshot.overflowed
        || snapshot
            .components
            .iter()
            .map(|x| x.current_requested)
            .sum::<u64>()
            != snapshot.total_current_requested
        || snapshot.peak_components.iter().sum::<u64>() != snapshot.global_peak_requested
        || snapshot
            .components
            .iter()
            .map(|x| x.current_overhead)
            .sum::<u64>()
            != snapshot.current_overhead
    {
        return Err("allocator ledger not reconciled or overflowed".into());
    }
    Ok(())
}
fn capture(name: &'static str, start: Instant, cpu_start: f64) -> Result<Phase> {
    let (cpu, working_set_bytes, process_peak_bytes) = process_metrics()?;
    let allocation = canonical_memory::snapshot();
    check(&allocation)?;
    let census = canonical_memory::census_snapshot();
    Ok(Phase {
        name,
        since_start_ms: start.elapsed().as_secs_f64() * 1000.0,
        cpu_since_start_ms: cpu - cpu_start,
        working_set_bytes,
        process_peak_bytes,
        allocation,
        census,
    })
}
fn subject() -> Subject {
    Subject {
        id: "memory-e12-observer".into(),
        readable_namespaces: BTreeSet::from([1, 2]),
    }
}
fn schema(version: u32) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version,
    }
}
fn decoded(r: &Registry, record: &Record) -> Result<Fact> {
    if record.namespace != 1
        || record.schema.namespace != 1
        || record.schema.id != 1
        || !(1..=2).contains(&record.schema.version)
    {
        return Err("unexpected fact identity/schema".into());
    }
    let values = r.decode_values(record.schema, &record.payload)?;
    let required = if record.schema.version == 1 { 3 } else { 4 };
    if values.len() != required {
        return Err("unexpected fact fields".into());
    }
    let uint = |i: usize| -> Result<u64> {
        match values.get(i) {
            Some(Some(Value::UInt(n))) => Ok(*n),
            _ => Err("invalid numeric field".into()),
        }
    };
    let text = match &values[2] {
        Some(Value::Utf8(s)) => s.clone(),
        _ => return Err("invalid text field".into()),
    };
    let extra = if required == 4 {
        match values[3] {
            Some(Value::UInt(n)) => Some(n),
            None => None,
            _ => return Err("invalid optional field".into()),
        }
    } else {
        None
    };
    Ok(Fact {
        id: record.id,
        revision: record.revision,
        schema_version: record.schema.version,
        action: uint(0)?,
        objective: uint(1)?,
        text,
        extra,
    })
}
fn graph_edges(g: &GraphView, start: u64, depth: usize, incoming: bool) -> Result<Vec<Edge>> {
    let walk = g.walk(
        &subject(),
        Reference {
            namespace: 1,
            id: start,
        },
        &WalkOptions {
            direction: if incoming {
                Direction::Incoming
            } else {
                Direction::Outgoing
            },
            relation_schema: Some(SchemaRef {
                namespace: 2,
                id: 1,
                version: 1,
            }),
            max_depth: depth,
            max_nodes: 10000,
            max_edges: 10000,
        },
    )?;
    let mut edges = walk
        .edges
        .into_iter()
        .map(|e| Edge {
            id: e.reference.id,
            source: e.source.id,
            kind: e.kind,
            target: e.target.id,
        })
        .collect::<Vec<_>>();
    edges.sort();
    Ok(edges)
}
fn verify(db: &Path, cfg: Config) -> Result<serde_json::Value> {
    let r = Registry::open(db)?;
    let snapshot = r.snapshot();
    let mut facts = Verifier::new(cfg);
    let mut heads = HeadVerifier::new(cfg);
    let mut edges = EdgeVerifier::new(cfg);
    if snapshot.len() as u64 != cfg.rows + cfg.graph_nodes() - 1 {
        return Err("unexpected snapshot count".into());
    }
    for current in snapshot.iter() {
        match current.namespace {
            1 => {
                heads.observe(&decoded(&r, current)?)?;
                for revision in r.history(1, current.id) {
                    if revision.deleted {
                        return Err("unexpected deleted revision".into());
                    }
                    facts.observe(&decoded(&r, &revision.record)?)?;
                }
            }
            2 => {
                if current.revision != 1 {
                    return Err("unexpected edge revision".into());
                }
                let e = r
                    .native_edge(Reference {
                        namespace: 2,
                        id: current.id,
                    })
                    .ok_or("edge binding missing")?;
                edges.observe(Edge {
                    id: current.id,
                    source: e.source.id,
                    kind: e.kind,
                    target: e.target.id,
                })?;
            }
            _ => return Err("unexpected namespace in database".into()),
        }
    }
    let history = facts.finish()?;
    let heads = heads.finish()?;
    let edges = edges.finish()?;
    let g = GraphView::new(&r, snapshot)?;
    for node in 1..=cfg.graph_nodes() {
        for incoming in [false, true] {
            if graph_edges(&g, node, 1, incoming)? != common::walk(cfg, node, 1, incoming) {
                return Err("full adjacency verification failed".into());
            }
        }
    }
    Ok(
        serde_json::json!({"config":cfg,"history_digest":history,"head_digest":heads,"edge_digest":edges,"full_adjacency_verified":true}),
    )
}
fn probe(db: &Path, cfg: Config, tracked: bool, layout: GraphLayout) -> Result<ProbeReport> {
    if canonical_memory::enabled() != tracked {
        return Err("selected global allocator differs from mode".into());
    }
    let _harness = canonical_memory::scope(Component::Harness);
    canonical_memory::clear_census();
    let mut phases = Vec::with_capacity(24);
    canonical_memory::reset_peak_window();
    let start = Instant::now();
    let (cpu_start, _, _) = process_metrics()?;
    phases.push(capture("base", start, cpu_start)?);
    let store = Store::open(db)?;
    phases.push(capture("store_open", start, cpu_start)?);
    drop(store);
    phases.push(capture("store_drop", start, cpu_start)?);
    let registry = Registry::open(db)?;
    phases.push(capture("registry_open", start, cpu_start)?);
    let snapshot = registry.snapshot();
    if snapshot.len() as u64 != cfg.rows + cfg.graph_nodes() - 1 {
        return Err("profile database count differs".into());
    }
    phases.push(capture("registry_snapshot", start, cpu_start)?);
    let mut query = QueryEngine::new(&registry, snapshot.clone())?;
    phases.push(capture("query_built", start, cpu_start)?);
    let graph = GraphView::new_with_layout(&registry, snapshot, layout)?;
    phases.push(capture("graph_built_snapshot_consumed", start, cpu_start)?);
    let fields = [schema(1), schema(2)];
    let filter = Predicate::Or(
        fields
            .iter()
            .map(|s| {
                Predicate::And(vec![
                    Predicate::Eq(
                        FieldRef {
                            schema: *s,
                            position: 0,
                        },
                        Value::UInt(2),
                    ),
                    Predicate::Eq(
                        FieldRef {
                            schema: *s,
                            position: 1,
                        },
                        Value::UInt(6),
                    ),
                ])
            })
            .collect(),
    );
    let selected = Query {
        namespaces: vec![1],
        schemas: fields.to_vec(),
        filter,
        order: Order::default(),
        limit: 1000,
        cursor: None,
    };
    let page = query.query(&subject(), &selected)?;
    if page.records.iter().map(|r| r.id).collect::<Vec<_>>() != common::matches(cfg, 2, 6) {
        return Err("profile selective query diverged".into());
    }
    phases.push(capture("selective_page_live", start, cpu_start)?);
    drop(page);
    phases.push(capture("selective_page_dropped", start, cpu_start)?);
    let ids = query.query_ids(&subject(), &selected)?;
    if ids.ids.iter().map(|r| r.id).collect::<Vec<_>>() != common::matches(cfg, 2, 6) {
        return Err("profile IDs projection diverged".into());
    }
    phases.push(capture("ids_page_live", start, cpu_start)?);
    drop(ids);
    phases.push(capture("ids_page_dropped", start, cpu_start)?);
    let projection = BTreeMap::from([(schema(1), vec![0, 1]), (schema(2), vec![0, 1, 3])]);
    let projected = query.query_projected(&subject(), &selected, &projection)?;
    if projected
        .rows
        .iter()
        .map(|r| r.identity.id)
        .collect::<Vec<_>>()
        != common::matches(cfg, 2, 6)
    {
        return Err("profile fields projection identities diverged".into());
    }
    for row in &projected.rows {
        let expected = common::fact(cfg, row.identity.id, cfg.latest(row.identity.id));
        let mut values = vec![
            Some(Value::UInt(expected.action)),
            Some(Value::UInt(expected.objective)),
        ];
        if expected.schema_version == 2 {
            values.push(expected.extra.map(Value::UInt));
        }
        if row.fields != values {
            return Err("profile field values diverged".into());
        }
    }
    phases.push(capture("fields_page_live", start, cpu_start)?);
    drop(projected);
    phases.push(capture("fields_page_dropped", start, cpu_start)?);

    let mut broad = Query {
        namespaces: vec![1],
        schemas: fields.to_vec(),
        filter: Predicate::True,
        order: Order::default(),
        limit: 16,
        cursor: None,
    };
    let mut expected_id = 1;
    for _ in 0..3 {
        let page = query.query(&subject(), &broad)?;
        for r in &page.records {
            if r.id != expected_id {
                return Err("cursor order diverged".into());
            }
            expected_id += 1;
        }
        broad.cursor = page.next;
        if broad.cursor.is_none() {
            break;
        }
    }
    if graph_edges(&graph, 1, 3, false)? != common::walk(cfg, 1, 3, false) {
        return Err("profile graph diverged".into());
    }
    phases.push(capture("cursor_queries_outputs_dropped", start, cpu_start)?);
    drop(graph);
    phases.push(capture("graph_drop", start, cpu_start)?);
    drop(query);
    phases.push(capture("query_drop", start, cpu_start)?);
    drop(registry);
    phases.push(capture("registry_drop", start, cpu_start)?);
    if !tracked
        && phases
            .iter()
            .any(|p| p.allocation.total_current_requested != 0 || !p.census.is_empty())
    {
        return Err("control must not report invented heap attribution".into());
    }
    Ok(ProbeReport {
        version: 2,
        tracked,
        hooks_compiled: cfg!(feature = "memory-profile"),
        graph_layout: match layout {
            GraphLayout::Legacy => "legacy",
            GraphLayout::Compact => "compact",
        },
        config: cfg,
        phases,
        component_names: ALL_COMPONENTS.iter().map(|c| c.name()).collect(),
        notes: vec![
            "Allocator requested bytes/origin and logical ownership census are different axes; never add them",
            "Global peak is linearized requested-byte ledger peak; sum peak_components equals that same peak event",
            "WorkingSet/Peak is process memory, not exact component/index memory; headers/allocator internals and in-flight calls differ",
            "Census values describe latest observed construction, not live totals after every drop; bytes/capacities and element storage overlap",
            "Only read/open/queries/drop; no append/checkpoint/flush/compact; driver must verify closed file hashes before/after",
            "Control and tracking run same lifecycle in separate processes; control built without feature/metadata headers",
            "Snapshot flow matches E11: clone handle to Query; original consumed by Graph, no extra retained snapshot",
            "Full semantic verification is a separate process outside captured windows",
            "E13 adds IDs/fields output phases; collector reserved24 slots, not the16-slot E12 baseline; construction/ownership differs as measured",
        ],
    })
}
pub fn run(tracked: bool) -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let mut db = None;
    let mut out = None;
    let mut rows = 100000;
    let mut revisions = 11;
    let mut verify_only = false;
    let mut layout = GraphLayout::Legacy;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--graph-layout" {
            layout = match args.get(i + 1).map(String::as_str) {
                Some("legacy") => GraphLayout::Legacy,
                Some("compact") => GraphLayout::Compact,
                _ => return Err("graph-layout legacy|compact required".into()),
            };
            i += 2;
            continue;
        }
        match args[i].as_str(){"--verify-only"=>{verify_only=true;i+=1;},"--db"|"--out"|"--rows"|"--revisions"=>{let key=&args[i];let value=args.get(i+1).ok_or("missing argument value")?;match key.as_str(){"--db"=>db=Some(PathBuf::from(value)),"--out"=>out=Some(PathBuf::from(value)),"--rows"=>rows=value.parse()?,_=>revisions=value.parse()?};i+=2;},_=>return Err("usage: --db EXISTING_CLOSED_DB --out NEW_OUTPUT_DIR --rows N --revisions V [--verify-only]".into())}
    }
    let db = db.ok_or("existing database required")?;
    if !db.is_dir() || !db.join("manifest.cbr").is_file() || !db.join("segments").is_dir() {
        return Err("refuse initializing a database during read-only profile".into());
    }
    let cfg = Config {
        rows,
        revisions,
        samples: 128,
    };
    cfg.validate()?;
    if verify_only {
        println!("{}", serde_json::to_string(&verify(&db, cfg)?)?);
        return Ok(());
    }
    let out = out.ok_or("new output directory required")?;
    fs::create_dir(&out)?;
    let report = probe(&db, cfg, tracked, layout)?;
    let encoded = serde_json::to_vec_pretty(&report)?;
    let path = out.join("profile.json");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    println!(
        "{}",
        serde_json::json!({"tracked":tracked,"profile":path,"phases":report.phases.len()})
    );
    Ok(())
}
