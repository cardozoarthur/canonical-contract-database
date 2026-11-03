use canonical_codec::{FRAME_HEADER, decode, decode_frame, encode_frame};
use canonical_core::{Error, MAX_FRAME_BYTES};
use std::panic::catch_unwind;

#[test]
fn every_single_bit_change_and_cut_is_rejected_without_panic() {
    let frame = encode_frame(7, b"deterministic bounded corruption corpus").unwrap();
    for cut in 0..frame.len() {
        let result = catch_unwind(|| decode_frame(&frame[..cut], 123));
        assert!(result.is_ok(), "panic at cut {cut}");
        assert!(result.unwrap().is_err(), "accepted cut {cut}");
    }
    for offset in 0..frame.len() {
        for bit in 0..8 {
            let mut changed = frame.clone();
            changed[offset] ^= 1 << bit;
            let result = catch_unwind(|| decode_frame(&changed, 123));
            assert!(result.is_ok(), "panic at offset {offset}, bit {bit}");
            assert!(
                result.unwrap().is_err(),
                "accepted offset {offset}, bit {bit}"
            );
        }
    }
    assert_eq!(
        decode_frame(&frame, 123).unwrap().1,
        b"deterministic bounded corruption corpus"
    );
}

#[test]
fn checksummed_hostile_lengths_are_rejected_before_requiring_a_body() {
    for length in [MAX_FRAME_BYTES as u32, u32::MAX - 1, u32::MAX] {
        let mut header = encode_frame(1, b"").unwrap();
        header.truncate(FRAME_HEADER);
        header[8..12].copy_from_slice(&length.to_le_bytes());
        let checksum = crc32fast::hash(&header[..20]);
        header[20..24].copy_from_slice(&checksum.to_le_bytes());
        assert!(matches!(
            catch_unwind(|| decode_frame(&header, 0)).unwrap(),
            Err(Error::TooLarge)
        ));
    }
}

#[test]
fn deterministic_random_bytes_and_malformed_varints_are_bounded() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for length in 0..256 {
        let bytes: Vec<u8> = (0..length)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        assert!(catch_unwind(|| decode_frame(&bytes, 0)).unwrap().is_err());
        // Generic payloads have no CRC: a syntactically valid random value is allowed.
        // For all outcomes the deserializer must remain bounded and must not panic.
        let _ = catch_unwind(|| decode::<Vec<Vec<u8>>>(&bytes)).unwrap();
    }
    for bytes in [
        vec![0xff; 10],
        vec![0xff; 64],
        vec![0xfe, 0xff, 0xff, 0xff, 0x0f],
        vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
    ] {
        assert!(catch_unwind(|| decode::<Vec<u8>>(&bytes)).unwrap().is_err());
        assert!(
            catch_unwind(|| decode::<Vec<Vec<u8>>>(&bytes))
                .unwrap()
                .is_err()
        );
    }
}
