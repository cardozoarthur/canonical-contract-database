//! Local failure-injection executable. Exit(73) skips Rust destructors at the requested stage.
use canonical_registry::{NewRecord, Registry, SchemaRef, Value};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err("usage: transaction-probe DB STAGE".into());
    }
    let stop = args[2].to_str().ok_or("invalid stage")?;
    let allowed = [
        "Prepared",
        "HeaderWritten",
        "BodyWritten",
        "BeforeSync",
        "Synced",
        "Published",
        "Ack",
    ];
    if !allowed.contains(&stop) {
        return Err("unknown stage".into());
    }
    let mut registry = Registry::open(&args[1])?;
    let records: Vec<_> = [10, 11]
        .into_iter()
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: SchemaRef {
                namespace: 1,
                id: 1,
                version: 1,
            },
            values: vec![Some(Value::UInt(id))],
        })
        .collect();
    let receipt = registry.create_batch_with_hook([9; 16], &records, |stage| {
        if format!("{stage:?}") == stop {
            std::process::exit(73);
        }
        Ok(())
    })?;
    println!(
        "ack sequence={} records={}",
        receipt.commit_sequence, receipt.records
    );
    Ok(())
}
