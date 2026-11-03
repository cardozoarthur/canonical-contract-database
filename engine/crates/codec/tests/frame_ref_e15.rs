use canonical_codec::*;
#[test]
fn borrowed_frame_verifies_identical_layout_and_returns_a_view() {
    let bytes = encode_frame(3, b"abc").unwrap();
    let (seq, payload, len) = decode_frame_ref(&bytes, 16).unwrap();
    assert_eq!((seq, payload, len), (3, &b"abc"[..], 31));
    assert_eq!(payload.as_ptr(), bytes[24..].as_ptr());
    assert_eq!(
        decode_frame(&bytes, 16).unwrap(),
        (seq, payload.to_vec(), len)
    );
}
#[test]
fn borrowed_and_owned_frame_errors_match_for_each_cut_mutation_and_trailing_frame() {
    let b = encode_frame(1, "unicode é bytes".as_bytes()).unwrap();
    for n in 0..b.len() {
        let mut bad = b.clone();
        bad[n] ^= 1;
        assert!(decode_frame_ref(&bad, 16).is_err());
        assert_eq!(
            decode_frame_ref(&bad, 16).err().unwrap().to_string(),
            decode_frame(&bad, 16).err().unwrap().to_string()
        );
        assert!(decode_frame_ref(&b[..n], 16).is_err());
    }
    let mut doubled = b.clone();
    doubled.extend_from_slice(&b);
    assert_eq!(decode_frame_ref(&doubled, 16).unwrap().2, b.len());
}
