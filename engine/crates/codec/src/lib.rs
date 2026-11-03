//! CBR v1: a bounded frame around an opaque canonical binary payload.
//! All integers in the envelope are little endian. CRC is integrity, not authentication.
use canonical_core::{Error, MAX_FRAME_BYTES, Result};

pub const FRAME_HEADER: usize = 24;
pub const FRAME_OVERHEAD: usize = FRAME_HEADER + 4;
pub const VERSION: u16 = 1;
const MAGIC: &[u8; 4] = b"CBRF";

pub fn encode_frame(sequence: u64, payload: &[u8]) -> Result<Vec<u8>> {
    let length = payload
        .len()
        .checked_add(FRAME_OVERHEAD)
        .ok_or(Error::TooLarge)?;
    if length > MAX_FRAME_BYTES {
        return Err(Error::TooLarge);
    }
    if sequence == 0 {
        return Err(Error::InvalidInput("sequence must be nonzero"));
    }
    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // kind: committed single blob
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(&out).to_le_bytes()); // header CRC before trusting length
    out.extend_from_slice(payload);
    out.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    Ok(out)
}

/// Decode one frame, leaving later frames untouched. Offset is used in diagnostics only.
pub fn decode_frame(input: &[u8], offset: u64) -> Result<(u64, Vec<u8>, usize)> {
    let (sequence, payload, total) = decode_frame_ref(input, offset)?;
    Ok((sequence, payload.to_vec(), total))
}

/// Verify the same complete frame and borrow its payload without copying it.
pub fn decode_frame_ref(input: &[u8], offset: u64) -> Result<(u64, &[u8], usize)> {
    let corrupt = || Error::Corruption { offset };
    let incomplete = || Error::IncompleteTail { offset };
    if input.len() < FRAME_HEADER {
        return Err(incomplete());
    }
    if &input[..4] != MAGIC {
        return Err(corrupt());
    }
    let version = u16::from_le_bytes(input[4..6].try_into().unwrap());
    if version != VERSION {
        return Err(Error::UnsupportedVersion(version));
    }
    if u16::from_le_bytes(input[6..8].try_into().unwrap()) != 1 {
        return Err(corrupt());
    }
    let checksum = u32::from_le_bytes(input[20..24].try_into().unwrap());
    if crc32fast::hash(&input[..20]) != checksum {
        return Err(corrupt());
    }
    let len = u32::from_le_bytes(input[8..12].try_into().unwrap()) as usize;
    let total = len.checked_add(FRAME_OVERHEAD).ok_or(Error::TooLarge)?;
    if total > MAX_FRAME_BYTES {
        return Err(Error::TooLarge);
    }
    if input.len() < total {
        return Err(incomplete());
    }
    let sequence = u64::from_le_bytes(input[12..20].try_into().unwrap());
    if sequence == 0 {
        return Err(corrupt());
    }
    let payload = &input[FRAME_HEADER..FRAME_HEADER + len];
    let crc = u32::from_le_bytes(input[FRAME_HEADER + len..total].try_into().unwrap());
    if crc32fast::hash(payload) != crc {
        return Err(corrupt());
    }
    Ok((sequence, payload, total))
}

/// Positional payload codec for schemas introduced in E02. No field names are stored.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>> {
    postcard::serialize_with_flavor(value, BoundedVec::new(MAX_FRAME_BYTES - FRAME_OVERHEAD))
        .map_err(|error| match error {
            postcard::Error::SerializeBufferFull => Error::TooLarge,
            _ => Error::InvalidInput("binary encoding failed"),
        })
}

// A storage flavor changes the output medium, not the Postcard wire representation.
// Check each write before reserving. Capped geometric growth avoids allocating a full
// frame for a small value or letting Vec's automatic growth overshoot the payload cap.
struct BoundedVec {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedVec {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }
    fn reserve(&mut self, additional: usize) -> postcard::Result<()> {
        if additional > self.limit - self.bytes.len() {
            return Err(postcard::Error::SerializeBufferFull);
        }
        let needed = self.bytes.len() + additional;
        if needed > self.bytes.capacity() {
            let capacity = needed
                .max(self.bytes.capacity().saturating_mul(2))
                .min(self.limit);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| postcard::Error::SerdeSerCustom)?;
        }
        Ok(())
    }
}
impl postcard::ser_flavors::Flavor for BoundedVec {
    type Output = Vec<u8>;
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.reserve(1)?;
        self.bytes.push(byte);
        Ok(())
    }
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.reserve(bytes.len())?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn finalize(self) -> postcard::Result<Vec<u8>> {
        Ok(self.bytes)
    }
}

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > MAX_FRAME_BYTES - FRAME_OVERHEAD {
        return Err(Error::TooLarge);
    }
    let (value, rest) = postcard::take_from_bytes(bytes)
        .map_err(|_| Error::InvalidInput("binary decoding failed"))?;
    if !rest.is_empty() {
        return Err(Error::InvalidInput("trailing binary payload"));
    }
    Ok(value)
}

/// Decode a borrowed or selective visitor with the same frame and trailing-byte bounds.
pub fn decode_seed<'de, S: serde::de::DeserializeSeed<'de>>(
    bytes: &'de [u8],
    seed: S,
) -> Result<S::Value> {
    if bytes.len() > MAX_FRAME_BYTES - FRAME_OVERHEAD {
        return Err(Error::TooLarge);
    }
    let mut decoder = postcard::Deserializer::from_bytes(bytes);
    let value = seed
        .deserialize(&mut decoder)
        .map_err(|_| Error::InvalidInput("binary decoding failed"))?;
    let rest = decoder
        .finalize()
        .map_err(|_| Error::InvalidInput("binary decoding failed"))?;
    if !rest.is_empty() {
        return Err(Error::InvalidInput("trailing binary payload"));
    }
    Ok(value)
}
