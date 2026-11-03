//! Public-API application example. Domain policy lives here, not in Registry.
use canonical_query::{FieldRef, Order, Predicate, Query, QueryEngine, Subject};
use canonical_registry::*;
use canonical_relations::{Direction, GraphView, WalkOptions};
use canonical_views::{CbrReader, CsvNestedPolicy, Format, Limits, Reader};
use std::{collections::BTreeSet, fs, path::Path};
pub type ExampleResult<T> = Result<T, Box<dyn std::error::Error>>;
pub const REQUEST: &[u8] =
    br#"{"task":102,"objective":1001,"contract":301,"before":1,"requested_after":2}"#;
pub const ARTIFACT_FILE: &str = "activation-request.json";
pub fn schema(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn key(id: u64) -> Reference {
    Reference { namespace: 1, id }
}
fn text(s: &str) -> Value {
    Value::Utf8(s.into())
}
fn input(id: u64, s: u64, v: Vec<Value>) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: schema(s),
        values: v.into_iter().map(Some).collect(),
    }
}
fn task(id: u64, objective: u64, state: u32) -> NewRecord {
    input(
        id,
        2,
        vec![
            text(&format!("task{id}")),
            Value::Ref(key(objective)),
            Value::Enum(state),
        ],
    )
}
fn edge(id: u64, s: u64, source: u64, target: u64) -> NewRecord {
    input(
        id,
        s,
        vec![Value::Relation {
            source: key(source),
            kind: s - 99,
            target: key(target),
        }],
    )
}
fn ensure(ok: bool, message: &'static str) -> ExampleResult<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
pub fn check_artifact(path: &Path) -> ExampleResult<()> {
    let metadata = fs::metadata(path)?;
    ensure(
        metadata.is_file() && metadata.len() == REQUEST.len() as u64,
        "artifact size/type mismatch",
    )?;
    ensure(fs::read(path)? == REQUEST, "artifact bytes mismatch")
}
fn subject() -> Subject {
    Subject {
        id: "workflow-reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    }
}
fn register(r: &mut Registry) -> ExampleResult<()> {
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("workflow{id}"),
            level: Level::Local,
        })?;
    }
    let states = || FieldType::Enum {
        variants: vec![1, 2, 3],
    };
    for (id, fields) in [
        (1, vec![("title", FieldType::Utf8)]),
        (
            2,
            vec![
                ("title", FieldType::Utf8),
                ("objective", FieldType::Ref),
                ("state", states()),
            ],
        ),
        (
            3,
            vec![
                ("title", FieldType::Utf8),
                ("from", states()),
                ("to", states()),
                ("descriptor", FieldType::Utf8),
            ],
        ),
        (
            4,
            vec![
                ("label", FieldType::Utf8),
                ("expected_revision", FieldType::UInt),
            ],
        ),
        (
            5,
            vec![
                ("filename", FieldType::Utf8),
                ("media", FieldType::Utf8),
                ("bytes", FieldType::Bytes),
            ],
        ),
        (
            6,
            vec![
                ("label", FieldType::Utf8),
                (
                    "outcome",
                    FieldType::Enum {
                        variants: vec![0, 1, 2],
                    },
                ),
                ("observed", FieldType::UInt),
            ],
        ),
        (
            7,
            vec![("label", FieldType::Utf8), ("criterion", FieldType::Utf8)],
        ),
        (8, vec![("label", FieldType::Utf8)]),
    ] {
        r.register_schema(Schema {
            reference: schema(id),
            fields: fields
                .into_iter()
                .map(|(label, kind)| Field {
                    label: label.into(),
                    kind,
                    optional: false,
                })
                .collect(),
        })?;
    }
    r.register_concept(Concept {
        namespace: 1,
        id: 1,
        version: 1,
        contract: b"application contract: planned -> active after evidence validation".to_vec(),
        label: Some("activation".into()),
    })?;
    // Each native relation has a distinct schema/kind and typed endpoints.
    for (s, source, target) in [
        (100, 2, 1),
        (101, 4, 2),
        (102, 4, 3),
        (103, 2, 2),
        (104, 6, 5),
        (105, 7, 6),
        (106, 5, 4),
    ] {
        r.register_schema(Schema {
            reference: schema(s),
            fields: vec![Field {
                label: "relation".into(),
                kind: FieldType::Relation,
                optional: false,
            }],
        })?;
        r.register_relation_rule(RelationRule {
            schema: schema(s),
            kind: s - 99,
            source_schema: schema(source),
            target_schema: schema(target),
        })?;
    }
    Ok(())
}
/// Application policy: enum membership alone is not a state machine.
pub fn check_transition(r: &Registry, task_id: u64, from: u32, to: u32) -> ExampleResult<()> {
    let t = r.decode_record(1, task_id)?;
    let c = r.decode_record(1, 301)?;
    ensure(
        t[2] == Some(Value::Enum(from))
            && c[1] == Some(Value::Enum(from))
            && c[2] == Some(Value::Enum(to)),
        "application transition denied",
    )
}
fn tasks(r: &Registry, state: Option<u32>) -> ExampleResult<Vec<Record>> {
    let mut predicates = vec![Predicate::Eq(
        FieldRef {
            schema: schema(2),
            position: 1,
        },
        Value::Ref(key(1001)),
    )];
    if let Some(state) = state {
        predicates.push(Predicate::Eq(
            FieldRef {
                schema: schema(2),
                position: 2,
            },
            Value::Enum(state),
        ));
    }
    let mut q = QueryEngine::new(r, r.snapshot())?;
    Ok(q.query(
        &subject(),
        &Query {
            namespaces: vec![1],
            schemas: vec![schema(2)],
            filter: Predicate::And(predicates),
            order: Order::default(),
            limit: 100,
            cursor: None,
        },
    )?
    .records)
}
/// Execute the real precondition checker, then atomically publish task+audit records.
/// The filesystem artifact is prepared first; it is not part of the database transaction.
pub fn run(database: &Path, evidence: &Path) -> ExampleResult<String> {
    ensure(!database.exists(), "run requires a new database path")?;
    ensure(!evidence.exists(), "run requires a new evidence directory")?;
    fs::create_dir_all(evidence)?;
    let mut r = Registry::open(database)?;
    register(&mut r)?;
    let mut initial = vec![
        input(1001, 1, vec![text("objectiveA")]),
        input(1002, 1, vec![text("objectiveB")]),
        task(101, 1001, 1),
        task(102, 1001, 1),
        task(103, 1001, 3),
        task(104, 1002, 1),
        input(
            301,
            3,
            vec![
                text("activate"),
                Value::Enum(1),
                Value::Enum(2),
                text("application-owned precondition contract"),
            ],
        ),
        input(
            501,
            7,
            vec![
                text("activation request"),
                text("exact bytes and typed task preconditions"),
            ],
        ),
        input(901, 8, vec![text("plugin-v1")]),
    ];
    let mut private = task(105, 1001, 1);
    private.namespace = 2;
    initial.push(private);
    for (id, source, target) in [
        (2001, 101, 1001),
        (2002, 102, 1001),
        (2003, 103, 1001),
        (2004, 104, 1002),
    ] {
        initial.push(edge(id, 100, source, target));
    }
    let mut hidden = edge(2005, 100, 105, 1001);
    hidden.values = vec![Some(Value::Relation {
        source: Reference {
            namespace: 2,
            id: 105,
        },
        kind: 1,
        target: key(1001),
    })];
    initial.push(hidden);
    initial.extend([edge(2101, 103, 101, 102), edge(2102, 103, 102, 103)]);
    r.create_batch([1; 16], &initial)?;
    fs::write(evidence.join(ARTIFACT_FILE), REQUEST)?;
    // This count is derived from four successful independent checks, not writer input.
    let mut observed = 0;
    check_artifact(&evidence.join(ARTIFACT_FILE))?;
    observed += 1;
    check_transition(&r, 102, 1, 2)?;
    observed += 1;
    ensure(
        r.record(1, 102).is_some_and(|x| x.revision == 1),
        "precondition revision",
    )?;
    observed += 1;
    ensure(
        r.decode_record(1, 102)?[1] == Some(Value::Ref(key(1001))),
        "precondition objective",
    )?;
    observed += 1;
    let mut mutations = vec![Mutation::Update {
        record: task(102, 1001, 2),
        expected_revision: 1,
    }];
    for record in [
        input(401, 4, vec![text("activate102"), Value::UInt(1)]),
        input(
            701,
            5,
            vec![
                text(ARTIFACT_FILE),
                text("application/json"),
                Value::Bytes(REQUEST.to_vec()),
            ],
        ),
        input(
            601,
            6,
            vec![
                text("activation preconditions"),
                Value::Enum(1),
                Value::UInt(observed),
            ],
        ),
        edge(2201, 101, 401, 102),
        edge(2202, 102, 401, 301),
        edge(2203, 104, 601, 701),
        edge(2204, 105, 501, 601),
        edge(2205, 106, 701, 401),
    ] {
        mutations.push(Mutation::Create(record));
    }
    r.commit([2; 16], &mutations)?;
    // Versioned extension stays independent of the relation endpoints' schema.
    let mut v2 = r.schema(schema(8))?.clone();
    v2.reference.version = 2;
    v2.fields.push(Field {
        label: "extra".into(),
        kind: FieldType::UInt,
        optional: true,
    });
    r.register_schema(v2.clone())?;
    r.create(
        1,
        902,
        v2.reference,
        &[Some(text("plugin-v2")), Some(Value::UInt(7))],
    )?;
    let records = tasks(&r, None)?;
    let reader = Reader::new(&r, Limits::default())?;
    for (filename, format) in [
        ("tasks.json", Format::Json),
        (
            "tasks.csv",
            Format::Csv {
                schema: schema(2),
                nested: CsvNestedPolicy::JsonCell,
            },
        ),
        ("tasks.cbr", Format::Cbr),
    ] {
        let mut file = fs::File::create(evidence.join(filename))?;
        reader.write_many(format, records.iter().cloned().map(Ok), &mut file)?;
        file.sync_all()?;
    }
    r.flush_index()?;
    drop(r);
    verify_existing(database, evidence)
}
/// Re-run checks independently of persisted `outcome=passed`. No writes to the database.
pub fn verify_existing(database: &Path, evidence: &Path) -> ExampleResult<String> {
    let r = Registry::open(database)?;
    check_artifact(&evidence.join(ARTIFACT_FILE))?;
    ensure(
        r.decode_record(1, 701)?
            == vec![
                Some(text(ARTIFACT_FILE)),
                Some(text("application/json")),
                Some(Value::Bytes(REQUEST.to_vec())),
            ],
        "canonical artifact mismatch",
    )?;
    ensure(
        r.decode_record(1, 601)?
            == vec![
                Some(text("activation preconditions")),
                Some(Value::Enum(1)),
                Some(Value::UInt(4)),
            ],
        "stored result mismatch",
    )?;
    ensure(
        r.decode_record(1, 401)? == vec![Some(text("activate102")), Some(Value::UInt(1))],
        "action mismatch",
    )?;
    let old = r.revision(1, 102, 1).ok_or("missing before revision")?;
    ensure(
        r.decode_values(old.record.schema, &old.record.payload)? == task(102, 1001, 1).values,
        "before revision mismatch",
    )?;
    ensure(
        r.decode_record(1, 102)? == task(102, 1001, 2).values
            && r.record(1, 102).is_some_and(|x| x.revision == 2),
        "after revision mismatch",
    )?;
    ensure(
        r.decode_record(1, 301)?[1..3] == [Some(Value::Enum(1)), Some(Value::Enum(2))],
        "contract mismatch",
    )?;
    let receipt = r
        .transaction_receipt([2; 16])
        .ok_or("missing transaction receipt")?;
    ensure(
        receipt.records == 9,
        "audit transaction member count mismatch",
    )?;
    for id in [102, 401, 701, 601, 2201, 2202, 2203, 2204, 2205] {
        let revision = if id == 102 { 2 } else { 1 };
        ensure(
            r.revision(1, id, revision)
                .is_some_and(|x| x.commit_sequence == receipt.commit_sequence),
            "audit transaction not atomic",
        )?;
    }
    for (state, id) in [(1, 101), (2, 102), (3, 103)] {
        ensure(
            tasks(&r, Some(state))?
                .iter()
                .map(|x| x.id)
                .collect::<Vec<_>>()
                == vec![id],
            "state/objective filter mismatch",
        )?;
    }
    let records = tasks(&r, None)?;
    ensure(
        records.iter().map(|x| x.id).collect::<Vec<_>>() == vec![101, 102, 103],
        "authorized task identities mismatch",
    )?;
    let mut query = QueryEngine::new(&r, r.snapshot())?;
    ensure(
        query
            .query(
                &subject(),
                &Query {
                    namespaces: vec![2],
                    schemas: vec![schema(2)],
                    filter: Predicate::True,
                    order: Order::default(),
                    limit: 100,
                    cursor: None,
                },
            )
            .is_err(),
        "unauthorized namespace exposed",
    )?;
    let graph = GraphView::new(&r, r.snapshot())?;
    let incoming = graph.incoming(&subject(), key(1001), Some(schema(100)), 100)?;
    ensure(
        incoming
            .edges
            .iter()
            .map(|x| x.reference.id)
            .collect::<Vec<_>>()
            == vec![2001, 2002, 2003]
            && !incoming.has_more,
        "hidden endpoint exposed",
    )?;
    let walk = graph.walk(
        &subject(),
        key(101),
        &WalkOptions {
            direction: Direction::Outgoing,
            relation_schema: Some(schema(103)),
            max_depth: 2,
            max_nodes: 10,
            max_edges: 10,
        },
    )?;
    ensure(
        walk.edges
            .iter()
            .map(|x| x.reference.id)
            .collect::<Vec<_>>()
            == vec![2101, 2102]
            && !walk.truncated,
        "dependency traversal mismatch",
    )?;
    for (source, s, expected) in [
        (401, 101, 2201),
        (401, 102, 2202),
        (601, 104, 2203),
        (501, 105, 2204),
        (701, 106, 2205),
    ] {
        ensure(
            graph
                .outgoing(&subject(), key(source), Some(schema(s)), 10)?
                .edges
                .iter()
                .map(|x| x.reference.id)
                .collect::<Vec<_>>()
                == vec![expected],
            "evidence graph mismatch",
        )?;
    }
    for expected in [
        edge(2001, 100, 101, 1001),
        edge(2002, 100, 102, 1001),
        edge(2003, 100, 103, 1001),
        edge(2101, 103, 101, 102),
        edge(2102, 103, 102, 103),
        edge(2201, 101, 401, 102),
        edge(2202, 102, 401, 301),
        edge(2203, 104, 601, 701),
        edge(2204, 105, 501, 601),
        edge(2205, 106, 701, 401),
    ] {
        ensure(
            r.decode_record(1, expected.id)? == expected.values,
            "typed edge endpoints mismatch",
        )?;
    }
    ensure(
        r.decode_record(1, 901)? == vec![Some(text("plugin-v1"))]
            && r.decode_record(1, 902)? == vec![Some(text("plugin-v2")), Some(Value::UInt(7))],
        "versioned resource mismatch",
    )?;
    // CBR restores canonical records; JSON/CSV are independently checked exports.
    let decoded = CbrReader::new(fs::File::open(evidence.join("tasks.cbr"))?)
        .collect::<canonical_core::Result<Vec<_>>>()?;
    ensure(decoded == records, "CBR record roundtrip mismatch")?;
    let actual: serde_json::Value =
        serde_json::from_slice(&fs::read(evidence.join("tasks.json"))?)?;
    let expected:Vec<_>=[(101,1,1),(102,2,2),(103,1,3)].into_iter().map(|(id,rev,state)|serde_json::json!({"namespace":1,"id":id,"schema":{"namespace":1,"id":2,"version":1},"revision":rev,"fields":{"title":format!("task{id}"),"objective":{"namespace":1,"id":1001},"state":state}})).collect();
    ensure(
        actual == serde_json::json!(expected),
        "JSON independent oracle mismatch",
    )?;
    // CSV exact content oracle includes RFC4180 escaping of the Ref JsonCell.
    let quote = |cells: Vec<String>| {
        cells
            .into_iter()
            .map(|x| format!("\"{}\"", x.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(",")
            + "\r\n"
    };
    let mut csv = quote(
        [
            "namespace",
            "id",
            "schema_namespace",
            "schema_id",
            "schema_version",
            "revision",
            "field.title",
            "field.objective",
            "field.state",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
    );
    for (id, rev, state) in [(101, 1, 1), (102, 2, 2), (103, 1, 3)] {
        csv.push_str(&quote(vec![
            "1".into(),
            id.to_string(),
            "1".into(),
            "2".into(),
            "1".into(),
            rev.to_string(),
            format!("task{id}"),
            r#"{"namespace":1,"id":1001}"#.into(),
            state.to_string(),
        ]));
    }
    ensure(
        fs::read(evidence.join("tasks.csv"))? == csv.as_bytes(),
        "CSV independent oracle mismatch",
    )?;
    Ok(format!(
        "Workflow local checks: tasks=3 states=3 atomic_members={} artifact_bytes={} graph_dependency_edges=2 exports=JSON/CSV/CBR resource_versions=2; US10 external consumer is separate",
        receipt.records,
        REQUEST.len()
    ))
}
/// Native revision conflict is atomic; domain state rules are checked by the app.
pub fn verify_rejected_changes(database: &Path) -> ExampleResult<()> {
    let mut r = Registry::open(database)?;
    ensure(
        check_transition(&r, 101, 1, 3).is_err(),
        "invalid app transition accepted",
    )?;
    let count = r.snapshot().len();
    let history = r.history(1, 102).len();
    let result = r.commit(
        [3; 16],
        &[
            Mutation::Create(input(402, 4, vec![text("must-not-exist"), Value::UInt(1)])),
            Mutation::Update {
                record: task(102, 1001, 3),
                expected_revision: 1,
            },
        ],
    );
    ensure(
        matches!(result, Err(canonical_core::Error::Conflict)),
        "stale revision not rejected",
    )?;
    ensure(
        r.snapshot().len() == count
            && r.record(1, 402).is_none()
            && r.history(1, 102).len() == history,
        "partial stale transaction",
    )?;
    ensure(
        r.update(task(101, 1001, 99), 1).is_err(),
        "invalid enum accepted",
    )?;
    ensure(
        r.record(1, 101).is_some_and(|x| x.revision == 1),
        "invalid enum wrote data",
    )?;
    let mut changed = r.schema(schema(8))?.clone();
    changed.fields.clear();
    ensure(
        r.register_schema(changed).is_err(),
        "schema version redefinition accepted",
    )?;
    Ok(())
}
