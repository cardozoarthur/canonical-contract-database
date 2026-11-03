use canonical_codec::{decode_frame, encode_frame};
use canonical_core::{Error, MAX_FRAME_BYTES};

#[test]
fn exact_roundtrip_and_trailing_frame_length() {
    let frame = encode_frame(1, b"\0abc\xff").unwrap();
    let mut joined = frame.clone();
    joined.extend_from_slice(&frame);
    let (seq, payload, consumed) = decode_frame(&joined, 0).unwrap();
    assert_eq!(
        (seq, payload, consumed),
        (1, b"\0abc\xff".to_vec(), frame.len())
    );
}

#[test]
fn bounds_checked_before_allocation_and_no_partial_success() {
    assert!(matches!(
        encode_frame(1, &vec![0; MAX_FRAME_BYTES]),
        Err(Error::TooLarge)
    ));
    let frame = encode_frame(1, b"payload").unwrap();
    for n in 0..frame.len() {
        assert!(matches!(
            decode_frame(&frame[..n], 12),
            Err(Error::IncompleteTail { offset: 12 })
        ));
    }
    let mut bad = frame;
    bad[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        decode_frame(&bad, 0),
        Err(Error::TooLarge) | Err(Error::Corruption { .. })
    ));
}

#[test]
fn version_and_all_protected_bytes_are_checked() {
    let frame = encode_frame(1, b"abc").unwrap();
    for i in 0..frame.len() {
        let mut bad = frame.clone();
        bad[i] ^= 0x01;
        assert!(decode_frame(&bad, 0).is_err(), "byte {i}");
    }
    let mut unknown = frame;
    unknown[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        decode_frame(&unknown, 0),
        Err(Error::UnsupportedVersion(2))
    ));
}

#[test]
fn published_v1_fixture_is_stable_and_portable() {
    // Generated independently by Python struct.pack + zlib.crc32, not this encoder.
    let golden = include_bytes!("fixtures/blob-v1.cbr");
    assert_eq!(encode_frame(1, b"\0abc\xff").unwrap(), golden);
    assert_eq!(
        decode_frame(golden, 0).unwrap(),
        (1, b"\0abc\xff".to_vec(), golden.len())
    );
}

#[test]
fn valid_header_with_excessive_length_is_rejected_before_body() {
    let mut header = encode_frame(1, b"").unwrap();
    header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    let crc = crc32fast::hash(&header[..20]);
    header[20..24].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(decode_frame(&header, 0), Err(Error::TooLarge)));
}

#[test]
fn positional_codec_preserves_utf8_and_rejects_trailing_data() {
    let value = (true, 871u64, "ação 日本語 العربية", vec![0u8, 255]);
    let mut bytes = canonical_codec::encode(&value).unwrap();
    let decoded: (bool, u64, String, Vec<u8>) = canonical_codec::decode(&bytes).unwrap();
    assert_eq!(decoded, (true, 871, value.2.to_string(), value.3));
    bytes.push(0);
    assert!(canonical_codec::decode::<(bool, u64, String, Vec<u8>)>(&bytes).is_err());
}
