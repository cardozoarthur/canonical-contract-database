use canonical_query::*;
use canonical_registry::*;
use canonical_views::*;
use std::{collections::BTreeSet, fs};
fn sr() -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: export DB OUTPUT_DIR".into());
    }
    let mut r = Registry::open(&args[1])?;
    r.register_namespace(Namespace {
        id: 1,
        name: "views.example".into(),
        level: Level::Local,
    })?;
    r.register_schema(Schema {
        reference: sr(),
        fields: vec![
            Field {
                label: "text".into(),
                kind: FieldType::Utf8,
                optional: false,
            },
            Field {
                label: "number".into(),
                kind: FieldType::UInt,
                optional: false,
            },
        ],
    })
    .unwrap();
    r.create_batch(
        [1; 16],
        &[
            NewRecord {
                namespace: 1,
                id: 1,
                schema: sr(),
                values: vec![
                    Some(Value::Utf8("ação, \"oi\"\n日本語".into())),
                    Some(Value::UInt(u64::MAX)),
                ],
            },
            NewRecord {
                namespace: 1,
                id: 2,
                schema: sr(),
                values: vec![Some(Value::Utf8("العربية".into())), Some(Value::UInt(871))],
            },
        ],
    )?;
    let mut engine = QueryEngine::new(&r, r.snapshot())?;
    let who = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let records = engine
        .query(
            &who,
            &Query {
                namespaces: vec![1],
                schemas: vec![sr()],
                filter: Predicate::True,
                order: Order::default(),
                limit: 1000,
                cursor: None,
            },
        )?
        .records;
    let reader = Reader::new(&r, Limits::default())?;
    fs::create_dir_all(&args[2])?;
    for (name, format) in [
        ("rows.json", Format::Json),
        (
            "rows.csv",
            Format::Csv {
                schema: sr(),
                nested: CsvNestedPolicy::Reject,
            },
        ),
        ("rows.cbr", Format::Cbr),
    ] {
        let path = std::path::Path::new(&args[2]).join(name);
        let mut file = fs::File::create(path)?;
        let stats = reader.write_many(format, records.iter().cloned().map(Ok), &mut file)?;
        file.sync_all()?;
        println!("{name} records={} bytes={}", stats.records, stats.bytes);
    }
    Ok(())
}
