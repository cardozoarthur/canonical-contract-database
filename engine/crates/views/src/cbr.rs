use canonical_codec::{FRAME_HEADER, FRAME_OVERHEAD, decode_frame, encode_frame};
use canonical_core::{Error, MAX_FRAME_BYTES, Result};
use canonical_registry::Record;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

// Distinct from the CSR journal. Fixed magic, version then canonical Record in Postcard.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    magic: [u8; 4],
    version: u16,
    record: &'a Record,
}
#[derive(Deserialize)]
struct Envelope {
    magic: [u8; 4],
    version: u16,
    record: Record,
}
pub(super) fn write<W: Write>(record: &Record, sequence: u64, out: &mut W) -> Result<()> {
    let body = canonical_codec::encode(&EnvelopeRef {
        magic: *b"CVW1",
        version: 1,
        record,
    })?;
    out.write_all(&encode_frame(sequence, &body)?)?;
    Ok(())
}

pub struct CbrReader<R> {
    input: R,
    sequence: u64,
    offset: u64,
    done: bool,
}
impl<R: Read> CbrReader<R> {
    pub fn new(input: R) -> Self {
        Self {
            input,
            sequence: 1,
            offset: 0,
            done: false,
        }
    }
    fn next_record(&mut self) -> Result<Option<Record>> {
        let mut header = [0; FRAME_HEADER];
        loop {
            match self.input.read(&mut header[..1]) {
                Ok(0) => return Ok(None),
                Ok(_) => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        exact(&mut self.input, &mut header[1..], self.offset)?;
        match decode_frame(&header, self.offset) {
            Err(Error::IncompleteTail { .. }) => (),
            Err(e) => return Err(e),
            Ok(_) => {
                return Err(Error::Corruption {
                    offset: self.offset,
                });
            }
        }
        let sequence = u64::from_le_bytes(header[12..20].try_into().unwrap());
        if sequence != self.sequence {
            return Err(Error::Corruption {
                offset: self.offset,
            });
        }
        let size = (u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize)
            .checked_add(FRAME_OVERHEAD)
            .ok_or(Error::TooLarge)?;
        if size > MAX_FRAME_BYTES {
            return Err(Error::TooLarge);
        }
        let mut frame = Vec::with_capacity(size);
        frame.extend_from_slice(&header);
        frame.resize(size, 0);
        exact(&mut self.input, &mut frame[FRAME_HEADER..], self.offset)?;
        let (_, body, _) = decode_frame(&frame, self.offset)?;
        let envelope: Envelope = canonical_codec::decode(&body)?;
        if envelope.magic != *b"CVW1" {
            return Err(Error::Corruption {
                offset: self.offset,
            });
        }
        if envelope.version != 1 {
            return Err(Error::UnsupportedVersion(envelope.version));
        }
        if envelope.record.id == 0
            || envelope.record.revision == 0
            || envelope.record.schema.id == 0
            || envelope.record.schema.version == 0
        {
            return Err(Error::InvalidInput("invalid CBR view record identity"));
        }
        self.offset = self
            .offset
            .checked_add(size as u64)
            .ok_or(Error::TooLarge)?;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::TooLarge)?;
        Ok(Some(envelope.record))
    }
}
impl<R: Read> Iterator for CbrReader<R> {
    type Item = Result<Record>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
        }
    }
}
impl<R: Read> std::iter::FusedIterator for CbrReader<R> {}
fn exact<R: Read>(input: &mut R, bytes: &mut [u8], offset: u64) -> Result<()> {
    input.read_exact(bytes).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            Error::IncompleteTail { offset }
        } else {
            e.into()
        }
    })
}
