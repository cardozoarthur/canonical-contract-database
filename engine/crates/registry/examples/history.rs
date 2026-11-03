use canonical_registry::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os().nth(1).ok_or("usage: history DB")?;
    let mut registry = Registry::open(directory)?;
    registry.register_namespace(Namespace {
        id: 1,
        name: "history.example".into(),
        level: Level::Local,
    })?;
    let schema = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    registry.register_schema(Schema {
        reference: schema,
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })?;
    if registry.history(1, 1).is_empty() {
        registry.create_batch(
            [1; 16],
            &[NewRecord {
                namespace: 1,
                id: 1,
                schema,
                values: vec![Some(Value::UInt(10))],
            }],
        )?;
        registry.commit(
            [2; 16],
            &[Mutation::Update {
                record: NewRecord {
                    namespace: 1,
                    id: 1,
                    schema,
                    values: vec![Some(Value::UInt(20))],
                },
                expected_revision: 1,
            }],
        )?;
        registry.delete(1, 1, 2)?;
    }
    for version in registry.history(1, 1) {
        println!(
            "revision={} deleted={} commit={}",
            version.record.revision, version.deleted, version.commit_sequence
        );
    }
    println!("current_present={}", registry.record(1, 1).is_some());
    Ok(())
}
