use canonical_core::{Error, MAX_FRAME_BYTES};
use serde::de::{DeserializeSeed, Deserializer};
struct BorrowText;
impl<'de> DeserializeSeed<'de> for BorrowText {
    type Value = &'de str;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        serde::Deserialize::deserialize(d)
    }
}
#[test]
fn borrowed_seed_shares_input_and_keeps_complete_consumption_bounds() {
    let bytes = canonical_codec::encode(&"héllo").unwrap();
    let decoded = canonical_codec::decode_seed(&bytes, BorrowText).unwrap();
    assert_eq!(decoded, "héllo");
    assert!(decoded.as_ptr() >= bytes.as_ptr());
    assert!((decoded.as_ptr() as usize + decoded.len()) <= bytes.as_ptr() as usize + bytes.len());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        canonical_codec::decode_seed(&trailing, BorrowText),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        canonical_codec::decode_seed(&bytes[..bytes.len() - 1], BorrowText),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        canonical_codec::decode_seed(&vec![0; MAX_FRAME_BYTES], BorrowText),
        Err(Error::TooLarge)
    ));
}
