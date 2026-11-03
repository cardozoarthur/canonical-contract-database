#[path = "support/contract_workflow.rs"]
mod workflow;
fn main() -> workflow::ExampleResult<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 4 {
        return Err("usage: contract_workflow run|review DB EVIDENCE_DIR".into());
    }
    let report = match args[1].to_str() {
        Some("run") => workflow::run(
            std::path::Path::new(&args[2]),
            std::path::Path::new(&args[3]),
        )?,
        Some("review") => workflow::verify_existing(
            std::path::Path::new(&args[2]),
            std::path::Path::new(&args[3]),
        )?,
        _ => return Err("unknown mode".into()),
    };
    workflow::verify_rejected_changes(std::path::Path::new(&args[2]))?;
    println!("{report}");
    Ok(())
}
