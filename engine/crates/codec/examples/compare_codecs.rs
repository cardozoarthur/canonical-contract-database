//! Reproducible payload-size comparison; not a production performance benchmark.
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Fixture {
    operation: u64,
    file: u64,
    before: u64,
    after: u64,
    changed_range: u64,
}
fn main() {
    let fixture = Fixture {
        operation: 2,
        file: 871,
        before: 42,
        after: 43,
        changed_range: 15,
    };
    let cbr = canonical_codec::encode(&fixture).unwrap();
    assert_eq!(canonical_codec::decode::<Fixture>(&cbr).unwrap(), fixture);
    let msgpack = rmp_serde::to_vec(&fixture).unwrap();
    assert_eq!(rmp_serde::from_slice::<Fixture>(&msgpack).unwrap(), fixture);
    let json = serde_json::to_vec(&fixture).unwrap();
    assert_eq!(serde_json::from_slice::<Fixture>(&json).unwrap(), fixture);
    println!("fixture=operation2-file871-before42-after43-range15");
    println!("postcard_payload_bytes={}", cbr.len());
    println!("msgpack_positional_payload_bytes={}", msgpack.len());
    println!("json_named_payload_bytes={}", json.len());
    println!(
        "cbr_frame_bytes={}",
        canonical_codec::encode_frame(1, &cbr).unwrap().len()
    );
    println!(
        "empty_db_headers_bytes=32 (manifest16+segment16); filesystem allocation and lock excluded"
    );
    println!(
        "not measured: latency, throughput, CPU, RAM, indexes, history, tokens, Protobuf, embedded DB"
    );
}
