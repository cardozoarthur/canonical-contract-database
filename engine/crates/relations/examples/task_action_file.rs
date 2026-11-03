use canonical_query::Subject;
use canonical_registry::*;
use canonical_relations::*;
use std::collections::BTreeSet;
fn sr(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn key(id: u64) -> Reference {
    Reference { namespace: 1, id }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("usage: task_action_file DB")?;
    let mut r = Registry::open(directory)?;
    r.register_namespace(Namespace {
        id: 1,
        name: "relations.example".into(),
        level: Level::Local,
    })?;
    for id in 1..=5 {
        r.register_schema(Schema {
            reference: sr(id),
            fields: vec![Field {
                label: "value".into(),
                kind: if id >= 4 {
                    FieldType::Relation
                } else {
                    FieldType::UInt
                },
                optional: false,
            }],
        })
        .unwrap();
    }
    for (schema, kind, source, target) in [(4, 10, 1, 2), (5, 11, 2, 3)] {
        r.register_relation_rule(RelationRule {
            schema: sr(schema),
            kind,
            source_schema: sr(source),
            target_schema: sr(target),
        })?;
    }
    let nodes: Vec<_> = (1..=3)
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr(id),
            values: vec![Some(Value::UInt(id))],
        })
        .collect();
    r.create_batch([1; 16], &nodes)?;
    let subject = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let access = WriteAccess {
        subject: subject.clone(),
        writable_namespaces: BTreeSet::from([1]),
    };
    for (id, schema, kind, source, target) in [(100, 4, 10, 1, 2), (101, 5, 11, 2, 3)] {
        if r.history(1, id).is_empty() {
            link(
                &mut r,
                &access,
                NewRecord {
                    namespace: 1,
                    id,
                    schema: sr(schema),
                    values: vec![Some(Value::Relation {
                        source: key(source),
                        kind,
                        target: key(target),
                    })],
                },
                0,
            )?;
        }
    }
    let view = GraphView::new(&r, r.snapshot())?;
    for (name, start, direction) in [
        ("task_to_file", 1, Direction::Outgoing),
        ("file_to_task", 3, Direction::Incoming),
    ] {
        let walk = view.walk(
            &subject,
            key(start),
            &WalkOptions {
                direction,
                relation_schema: None,
                max_depth: 16,
                max_nodes: 100,
                max_edges: 100,
            },
        )?;
        println!(
            "{name} nodes={:?} edges={} truncated={}",
            walk.nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
            walk.edges.len(),
            walk.truncated
        );
    }
    Ok(())
}
