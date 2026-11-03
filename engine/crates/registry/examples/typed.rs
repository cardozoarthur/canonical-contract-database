use canonical_registry::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os().nth(1).ok_or("usage: typed DB")?;
    let mut registry = Registry::open(directory)?;
    registry.register_namespace(Namespace {
        id: 1,
        name: "example".into(),
        level: Level::Local,
    })?;
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    registry.register_schema(Schema {
        reference: sr,
        fields: vec![Field {
            label: "operation".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })?;
    if registry.record(1, 1).is_none() {
        registry.create(1, 1, sr, &[Some(Value::UInt(2))])?;
    }
    println!("{:?}", registry.decode_record(1, 1)?);
    Ok(())
}
