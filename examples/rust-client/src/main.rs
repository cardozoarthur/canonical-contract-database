use canonical_query::{Order, Predicate, Query, QueryEngine, Subject};
use canonical_registry::{Reference, Registry, SchemaRef};
use canonical_relations::{Direction, GraphView, WalkOptions};
use canonical_views::{CbrReader, CsvNestedPolicy, Format, Limits, Reader};
use std::{collections::BTreeSet, fs, io::Cursor, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: ccdb-public-client EXISTING_WORKFLOW_DB NEW_EXPORT_DIR".into());
    }
    let directory = PathBuf::from(&args[1]);
    // This client consumes the documented example schema, never private source modules.
    let task_schema = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    let registry = Registry::open(&directory)?;
    let fields = registry.schema(task_schema)?.fields.len();
    let subject = Subject {
        id: "independent-rust-client".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let mut engine = QueryEngine::new(&registry, registry.snapshot())?;
    let page = engine.query(
        &subject,
        &Query {
            namespaces: vec![1],
            schemas: vec![task_schema],
            filter: Predicate::True,
            order: Order::default(),
            limit: 1000,
            cursor: None,
        },
    )?;
    if page.next.is_some() || page.records.is_empty() {
        return Err(
            "example task dataset is missing or exceeds this client's documented page bound".into(),
        );
    }
    let ids: Vec<_> = page.records.iter().map(|fact| fact.id).collect();
    if ids != [101, 102, 103, 104]
        || page
            .records
            .iter()
            .any(|record| record.namespace != 1 || record.schema != task_schema)
    {
        return Err("public client saw unexpected identities or an unauthorized record".into());
    }
    if !ids.contains(&102) || registry.history(1, 102).len() < 2 {
        return Err(
            "task102 and its versioned history are required by the example data contract".into(),
        );
    }
    let graph = GraphView::new(&registry, registry.snapshot())?;
    let walk = graph.walk(
        &subject,
        Reference {
            namespace: 1,
            id: 102,
        },
        &WalkOptions {
            direction: Direction::Outgoing,
            relation_schema: None,
            max_depth: 3,
            max_nodes: 100,
            max_edges: 100,
        },
    )?;
    if walk.truncated || walk.nodes.len() < 2 || walk.edges.is_empty() {
        return Err("task102 has no complete bounded relationship walk".into());
    }
    // Reader has no independent ACL: only authorized Query output reaches exports.
    let records: Vec<_> = page.records.clone();
    let reader = Reader::new(&registry, Limits::default())?;
    let out = PathBuf::from(&args[2]);
    if out.exists() {
        return Err("export directory must be new".into());
    }
    fs::create_dir_all(&out)?;
    let mut exports = Vec::new();
    for (name, format) in [
        ("tasks.json", Format::Json),
        (
            "tasks.csv",
            Format::Csv {
                schema: task_schema,
                nested: CsvNestedPolicy::JsonCell,
            },
        ),
        ("tasks.cbr", Format::Cbr),
    ] {
        let mut bytes = Vec::new();
        let stats = reader.write_many(format, records.iter().cloned().map(Ok), &mut bytes)?;
        if stats.records as usize != records.len() || bytes.is_empty() {
            return Err("export omitted records".into());
        }
        if name.ends_with("json") {
            let _: serde_json::Value = serde_json::from_slice(&bytes)?;
        }
        if name.ends_with("cbr") {
            let decoded: Result<Vec<_>, _> = CbrReader::new(Cursor::new(&bytes)).collect();
            if decoded? != records {
                return Err("CBR round-trip differs from authorized records".into());
            }
        }
        fs::write(out.join(name), &bytes)?;
        exports.push(serde_json::json!({"file":name,"records":stats.records,"bytes":bytes.len()}));
    }
    let revision = registry
        .record(1, 102)
        .ok_or("task102 head absent")?
        .revision;
    drop(graph);
    drop(engine);
    drop(registry);
    let reopened = Registry::open(&directory)?;
    if reopened
        .record(1, 102)
        .ok_or("task102 absent after reopen")?
        .revision
        != revision
    {
        return Err("head changed across reopen without a writer".into());
    }
    let report = serde_json::json!({
        "accepted":true,"story":"US-10","public_crates_only":true,
        "task_schema_fields":fields,"task_ids":ids,"task102_revision":revision,
        "task102_history_revisions":reopened.history(1,102).len(),
        "graph_nodes":walk.nodes.len(),"graph_edges":walk.edges.len(),
        "exports":exports,"reopened":true,"foundry_integration":false,
        "limits":"Local trusted Registry owner, authorized Query before Reader, experimental Rust API, bounded example dataset"
    });
    fs::write(
        out.join("client-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}
