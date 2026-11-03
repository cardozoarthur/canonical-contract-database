use canonical_codec::{FRAME_OVERHEAD, encode};
use canonical_core::{Error, MAX_FRAME_BYTES};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use std::cell::Cell;

struct Generated<'a> {
    count: usize,
    visited: &'a Cell<usize>,
}
impl Serialize for Generated<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.count))?;
        for _ in 0..self.count {
            self.visited.set(self.visited.get() + 1);
            seq.serialize_element(&false)?;
        }
        seq.end()
    }
}

#[test]
fn encoder_stops_before_consuming_all_of_oversized_generated_input() {
    let limit = MAX_FRAME_BYTES - FRAME_OVERHEAD;
    let visited = Cell::new(0);
    let value = Generated {
        count: limit + 1024,
        visited: &visited,
    };
    assert!(matches!(encode(&value), Err(Error::TooLarge)));
    assert!(
        visited.get() <= limit + 1,
        "visited {} elements past bounded output",
        visited.get()
    );
    assert!(visited.get() < value.count);
}

#[test]
fn exact_payload_limit_is_accepted_and_one_extra_byte_is_rejected() {
    let limit = MAX_FRAME_BYTES - FRAME_OVERHEAD;
    // This length uses a four-byte Postcard varint sequence prefix.
    let mut bytes = vec![0u8; limit - 4];
    let encoded = encode(&bytes).unwrap();
    assert_eq!(encoded.len(), limit);
    assert!(encoded.capacity() <= limit);
    bytes.push(0);
    assert!(matches!(encode(&bytes), Err(Error::TooLarge)));
}

#[test]
fn small_values_do_not_reserve_a_full_frame_and_serializer_errors_remain_invalid_input() {
    let output = encode(&true).unwrap();
    assert_eq!(output, [1]);
    assert!(output.capacity() < 1024);
    struct Invalid;
    impl Serialize for Invalid {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("invalid test input"))
        }
    }
    assert!(matches!(encode(&Invalid), Err(Error::InvalidInput(_))));
}
