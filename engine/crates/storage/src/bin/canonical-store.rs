use canonical_storage::Store;
use std::{env, fs, process};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().collect();
    match args.get(1).and_then(|s| s.to_str()) {
        Some("append") if args.len() == 4 => {
            // Reject oversized inputs before loading them.
            let max = canonical_core::MAX_FRAME_BYTES - canonical_codec::FRAME_OVERHEAD;
            if fs::metadata(&args[3])?.len() > max as u64 {
                return Err(canonical_core::Error::TooLarge.into());
            }
            let bytes = fs::read(&args[3])?;
            let receipt = Store::open(&args[2])?.append(&bytes)?;
            println!("durable sequence={}", receipt.sequence);
        }
        Some("read") if args.len() == 5 => {
            let sequence: u64 = args[3].to_str().ok_or("invalid sequence")?.parse()?;
            let db = Store::open(&args[2])?;
            let snap = db.snapshot();
            let entry = snap
                .iter()
                .find(|e| e.sequence == sequence)
                .ok_or("not found")?;
            fs::write(&args[4], &entry.payload)?;
        }
        _ => return Err("usage: canonical-store append DB INPUT | read DB SEQUENCE OUTPUT".into()),
    }
    Ok(())
}
