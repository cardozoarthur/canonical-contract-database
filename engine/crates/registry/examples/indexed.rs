use canonical_registry::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os().nth(1).ok_or("usage: indexed DB")?;
    let mut r = Registry::open(directory)?;
    let status = r.index_status();
    r.register_namespace(Namespace {
        id: 1,
        name: "indexed.example".into(),
        level: Level::Local,
    })?;
    let schema = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: schema,
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })?;
    if r.history(1, 1).is_empty() {
        r.create(1, 1, schema, &[Some(Value::UInt(871))])?;
    }
    r.flush_index()?;
    let result = r.lookup(1, 1, None)?;
    println!(
        "loaded={} rebuilt={} revision={}",
        status.loaded,
        status.rebuilt,
        result.revision.unwrap().record.revision
    );
    println!(
        "index_probes={} point_reads={} point_bytes={} scanned_frames={} decoded_members={}",
        result.metrics.index_probes,
        result.metrics.point_reads,
        result.metrics.point_bytes,
        result.metrics.frames_scanned,
        result.metrics.decoded_members
    );
    Ok(())
}
