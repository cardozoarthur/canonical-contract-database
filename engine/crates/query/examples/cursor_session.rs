// Process-restart probe: an old cursor must not become valid in a new snapshot/session.
use canonical_core::Error;
use canonical_query::*;
use canonical_registry::*;
use std::{collections::BTreeSet, fs};
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 {
        return Err("usage: cursor_session DB TOKEN_FILE issue|verify".into());
    }
    let mut r = Registry::open(&args[1])?;
    r.register_namespace(Namespace {
        id: 1,
        name: "cursor.session".into(),
        level: Level::Local,
    })?;
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })?;
    let records: Vec<_> = [1, 2]
        .into_iter()
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr,
            values: vec![Some(Value::UInt(id * 10))],
        })
        .collect();
    r.create_batch([1; 16], &records)?;
    let verify = args[3] == "verify";
    if verify {
        r.update(
            NewRecord {
                namespace: 1,
                id: 2,
                schema: sr,
                values: vec![Some(Value::UInt(99))],
            },
            1,
        )?;
    }
    let mut engine = QueryEngine::new(&r, r.snapshot())?;
    let subject = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let mut request = Query {
        namespaces: vec![1],
        schemas: vec![sr],
        filter: Predicate::True,
        order: Order::default(),
        limit: 1,
        cursor: None,
    };
    let fresh = engine.query(&subject, &request)?.next.unwrap();
    if !verify {
        fs::write(&args[2], fresh.to_bytes())?;
        println!("issued_cursor_bytes={}", fresh.to_bytes().len());
        return Ok(());
    }
    request.cursor = Some(Cursor::from_bytes(
        fs::read(&args[2])?.as_slice().try_into()?,
    ));
    match engine.query(&subject, &request) {
        Err(Error::InvalidCursor) => {
            println!("old_process_cursor_rejected=true");
            Ok(())
        }
        Err(error) => Err(error.into()),
        Ok(_) => Err("old cursor accepted after restart with changed snapshot".into()),
    }
}
