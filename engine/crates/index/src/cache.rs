//! Shared bounded cache reader and streamed encoders. No cache is authoritative.
use super::*;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Write},
    path::PathBuf,
};
fn corrupt(offset: u64) -> Error {
    Error::Corruption { offset }
}
fn bounded_count(count: u64) -> Result<()> {
    if count > MAX_INDEX_ENTRIES {
        Err(Error::TooLarge)
    } else {
        Ok(())
    }
}
fn legacy_size(count: u64) -> Result<u64> {
    bounded_count(count)?;
    let size = count
        .checked_mul(ENTRY as u64)
        .and_then(|n| n.checked_add((HEADER + FOOTER) as u64))
        .ok_or(Error::TooLarge)?;
    if size > MAX_INDEX_BYTES {
        Err(Error::TooLarge)
    } else {
        Ok(size)
    }
}
fn grouped_size(count: u64, groups: u64) -> Result<u64> {
    bounded_count(count)?;
    let size = count
        .checked_mul(32)
        .and_then(|n| groups.checked_mul(24).and_then(|g| n.checked_add(g)))
        .and_then(|n| n.checked_add(44))
        .ok_or(Error::TooLarge)?;
    if size > MAX_INDEX_BYTES {
        Err(Error::TooLarge)
    } else {
        Ok(size)
    }
}
struct Reader {
    file: BufReader<File>,
    length: u64,
    offset: u64,
    body_end: u64,
    checksum: crc32fast::Hasher,
    version: u16,
    format: CacheFormat,
    watermark: u64,
    count: u64,
    read: u64,
    groups: u64,
    groups_read: u64,
    remaining: u64,
    identity: (u64, u64),
    previous: Option<IndexEntry>,
}
impl Reader {
    fn open(path: &Path, expected_watermark: u64) -> Result<Self> {
        let file = File::open(path)?;
        let length = file.metadata()?.len();
        if length > MAX_INDEX_BYTES {
            return Err(Error::TooLarge);
        }
        if length < (HEADER + FOOTER) as u64 {
            return Err(corrupt(0));
        }
        let mut r = Self {
            file: BufReader::new(file),
            length,
            offset: 0,
            body_end: length - 4,
            checksum: crc32fast::Hasher::new(),
            version: 0,
            format: CacheFormat::Legacy,
            watermark: 0,
            count: 0,
            read: 0,
            groups: 0,
            groups_read: 0,
            remaining: 0,
            identity: (0, 0),
            previous: None,
        };
        let mut h = [0; HEADER];
        r.bytes(&mut h)?;
        if &h[..8] != MAGIC {
            return Err(corrupt(0));
        }
        r.version = u16::from_le_bytes(h[8..10].try_into().unwrap());
        r.format = match r.version {
            1 | 2 => {
                if h[10..16] != [0; 6] {
                    return Err(corrupt(10));
                }
                CacheFormat::Legacy
            }
            3 => {
                if h[11..16] != [0; 5] {
                    return Err(corrupt(11));
                }
                match h[10] {
                    1 => CacheFormat::Grouped,
                    2 => CacheFormat::Delta,
                    _ => return Err(corrupt(10)),
                }
            }
            n => return Err(Error::UnsupportedVersion(n)),
        };
        r.watermark = u64::from_le_bytes(h[16..24].try_into().unwrap());
        if r.watermark != expected_watermark {
            return Err(Error::Conflict);
        }
        r.count = u64::from_le_bytes(h[24..32].try_into().unwrap());
        bounded_count(r.count)?;
        if r.version < 3 {
            if legacy_size(r.count)? != length {
                return Err(corrupt(24));
            }
        } else {
            r.groups = r.u64()?;
            if r.groups > r.count || ((r.groups == 0) != (r.count == 0)) {
                return Err(corrupt(32));
            }
            if r.format == CacheFormat::Grouped && grouped_size(r.count, r.groups)? != length {
                return Err(corrupt(24));
            }
        }
        Ok(r)
    }
    fn bytes(&mut self, bytes: &mut [u8]) -> Result<()> {
        let end = self
            .offset
            .checked_add(bytes.len() as u64)
            .ok_or(Error::TooLarge)?;
        if end > self.body_end {
            return Err(corrupt(self.offset));
        }
        self.file.read_exact(bytes)?;
        self.checksum.update(bytes);
        self.offset = end;
        Ok(())
    }
    fn u64(&mut self) -> Result<u64> {
        let mut b = [0; 8];
        self.bytes(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn varint(&mut self) -> Result<u64> {
        let start = self.offset;
        let mut n = 0;
        for i in 0..10 {
            let mut b = [0];
            self.bytes(&mut b)?;
            let b = b[0];
            if i == 9 && b > 1 {
                return Err(corrupt(start));
            }
            n |= ((b & 127) as u64) << (i * 7);
            if b & 128 == 0 {
                if i > 0 && b == 0 {
                    return Err(corrupt(start));
                }
                return Ok(n);
            }
        }
        Err(corrupt(start))
    }
    fn group(&mut self) -> Result<()> {
        let start = self.offset;
        if self.groups_read >= self.groups {
            return Err(corrupt(start));
        }
        let (ns, id, n) = if self.format == CacheFormat::Grouped {
            (self.u64()?, self.u64()?, self.u64()?)
        } else {
            let delta = self.varint()?;
            let id = self.varint()?;
            let n = self.varint()?;
            let ns = self.identity.0.checked_add(delta).ok_or(corrupt(start))?;
            let id = if delta == 0 {
                self.identity.1.checked_add(id).ok_or(corrupt(start))?
            } else {
                id
            };
            (ns, id, n)
        };
        if id == 0
            || n == 0
            || n > self.count - self.read
            || (self.groups_read > 0 && (ns, id) <= self.identity)
        {
            return Err(corrupt(start));
        }
        self.identity = (ns, id);
        self.remaining = n;
        self.groups_read += 1;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<IndexEntry>> {
        if self.read == self.count {
            if self.version == 3 && (self.remaining != 0 || self.groups_read != self.groups) {
                return Err(corrupt(self.offset));
            }
            return Ok(None);
        }
        let start = self.offset;
        let entry = if self.version < 3 {
            let mut b = [0; ENTRY];
            self.bytes(&mut b)?;
            if b[44] > 1 || b[45..48] != [0; 3] {
                return Err(corrupt(start + 44));
            }
            IndexEntry {
                key: IndexKey {
                    namespace: u64::from_le_bytes(b[..8].try_into().unwrap()),
                    id: u64::from_le_bytes(b[8..16].try_into().unwrap()),
                    revision: u64::from_le_bytes(b[16..24].try_into().unwrap()),
                },
                location: Location {
                    commit_sequence: u64::from_le_bytes(b[24..32].try_into().unwrap()),
                    frame_offset: u64::from_le_bytes(b[32..40].try_into().unwrap()),
                    member: u32::from_le_bytes(b[40..44].try_into().unwrap()),
                    deleted: b[44] == 1,
                },
            }
        } else {
            if self.remaining == 0 {
                self.group()?;
            }
            let (revision, sequence, offset, member, deleted) =
                if self.format == CacheFormat::Grouped {
                    let mut b = [0; 32];
                    self.bytes(&mut b)?;
                    if b[28] > 1 || b[29..32] != [0; 3] {
                        return Err(corrupt(self.offset - 4));
                    }
                    (
                        u64::from_le_bytes(b[..8].try_into().unwrap()),
                        u64::from_le_bytes(b[8..16].try_into().unwrap()),
                        u64::from_le_bytes(b[16..24].try_into().unwrap()),
                        u32::from_le_bytes(b[24..28].try_into().unwrap()),
                        b[28] == 1,
                    )
                } else {
                    let mut revision = self.varint()?;
                    let mut sequence = self.varint()?;
                    let mut offset = self.varint()?;
                    let flags = self.varint()?;
                    if flags > 2047 {
                        return Err(corrupt(start));
                    }
                    if let Some(prior) = self
                        .previous
                        .filter(|p| (p.key.namespace, p.key.id) == self.identity)
                    {
                        revision = prior
                            .key
                            .revision
                            .checked_add(revision)
                            .ok_or(corrupt(start))?;
                        sequence = prior
                            .location
                            .commit_sequence
                            .checked_add(sequence)
                            .ok_or(corrupt(start))?;
                        offset = prior
                            .location
                            .frame_offset
                            .checked_add(offset)
                            .ok_or(corrupt(start))?;
                    }
                    (
                        revision,
                        sequence,
                        offset,
                        (flags >> 1) as u32,
                        flags & 1 != 0,
                    )
                };
            self.remaining -= 1;
            IndexEntry {
                key: IndexKey {
                    namespace: self.identity.0,
                    id: self.identity.1,
                    revision,
                },
                location: Location {
                    commit_sequence: sequence,
                    frame_offset: offset,
                    member,
                    deleted,
                },
            }
        };
        self.check_entry(entry, start)?;
        self.previous = Some(entry);
        self.read += 1;
        Ok(Some(entry))
    }
    fn check_entry(&self, entry: IndexEntry, start: u64) -> Result<()> {
        let IndexEntry { key, location } = entry;
        if key.id == 0
            || key.revision == 0
            || location.commit_sequence == 0
            || location.commit_sequence > self.watermark
            || location.frame_offset < 16
            || location.member >= 1024
        {
            return Err(corrupt(start));
        }
        if let Some(prior) = self.previous {
            if key <= prior.key {
                return Err(corrupt(start));
            }
            if (key.namespace, key.id) == (prior.key.namespace, prior.key.id) {
                if location.commit_sequence <= prior.location.commit_sequence
                    || location.frame_offset <= prior.location.frame_offset
                    || (self.version == 1
                        && prior.key.revision.checked_add(1) != Some(key.revision))
                {
                    return Err(corrupt(start));
                }
            } else if self.version == 1 && key.revision != 1 {
                return Err(corrupt(start));
            }
        } else if self.version == 1 && key.revision != 1 {
            return Err(corrupt(start));
        }
        Ok(())
    }
    fn finish(mut self) -> Result<()> {
        if self.read != self.count || self.offset != self.body_end {
            return Err(corrupt(self.offset));
        }
        let mut footer = [0; 4];
        self.file.read_exact(&mut footer)?;
        if self.checksum.finalize() != u32::from_le_bytes(footer) {
            return Err(corrupt(self.length - 4));
        }
        let mut extra = [0];
        if self.file.read(&mut extra)? != 0 {
            return Err(corrupt(self.length));
        }
        Ok(())
    }
}
pub(super) fn load(path: &Path, watermark: u64) -> Result<PrimaryIndex> {
    let mut reader = Reader::open(path, watermark)?;
    let mut index = PrimaryIndex::new();
    while let Some(entry) = reader.next()? {
        index
            .insert_inner(entry, reader.version != 1)
            .map_err(|_| corrupt(reader.offset))?;
    }
    reader.finish()?;
    Ok(index)
}
fn entries(index: &PrimaryIndex) -> impl Iterator<Item = Result<IndexEntry>> + Clone + '_ {
    index.history.iter().map(|(key, location)| {
        Ok(IndexEntry {
            key: *key,
            location: *location,
        })
    })
}
pub(super) fn validate(
    path: &Path,
    watermark: u64,
    expected: &PrimaryIndex,
) -> Result<CacheValidation> {
    compare_entries(path, watermark, expected.len() as u64, entries(expected))
}
pub(super) fn validate_entries<I>(
    path: &Path,
    watermark: u64,
    count: u64,
    source: I,
) -> Result<CacheValidation>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    let mut reader = Reader::open(path, watermark)?;
    // Cache wire bounds do not constrain the authoritative journal cardinality.
    // Count alone proves a negative result; no authority iteration is necessary,
    // but the entire cache must still pass structural and CRC validation.
    if reader.count != count {
        while reader.next()?.is_some() {}
        reader.finish()?;
        return Ok(CacheValidation::ContentMismatch);
    }
    inspect(source.clone(), count, watermark)?;
    compare_reader(reader, count, Checked::new(source, count, watermark))
}
fn compare_entries<I: Iterator<Item = Result<IndexEntry>>>(
    path: &Path,
    watermark: u64,
    count: u64,
    expected: I,
) -> Result<CacheValidation> {
    compare_reader(Reader::open(path, watermark)?, count, expected)
}
fn compare_reader<I: Iterator<Item = Result<IndexEntry>>>(
    mut reader: Reader,
    count: u64,
    mut expected: I,
) -> Result<CacheValidation> {
    let mut matched = reader.count == count;
    let mut source_error = None;
    while let Some(entry) = reader.next()? {
        match expected.next() {
            Some(Ok(wanted)) => matched &= wanted == entry,
            Some(Err(error)) => {
                source_error = Some(error);
                matched = false;
            }
            None => matched = false,
        }
    }
    for extra in expected {
        matched = false;
        if let Err(error) = extra {
            source_error = Some(error);
        }
    }
    reader.finish()?;
    if let Some(error) = source_error {
        return Err(error);
    }
    Ok(if matched {
        CacheValidation::Match
    } else {
        CacheValidation::ContentMismatch
    })
}

#[derive(Clone)]
struct Checked<I> {
    inner: I,
    count: u64,
    expected: u64,
    watermark: u64,
    previous: Option<IndexEntry>,
    done: bool,
}
impl<I> Checked<I> {
    fn new(inner: I, expected: u64, watermark: u64) -> Self {
        Self {
            inner,
            count: 0,
            expected,
            watermark,
            previous: None,
            done: false,
        }
    }
}
impl<I: Iterator<Item = Result<IndexEntry>>> Iterator for Checked<I> {
    type Item = Result<IndexEntry>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let entry = match self.inner.next() {
            Some(Ok(entry)) => entry,
            Some(Err(error)) => {
                self.done = true;
                return Some(Err(error));
            }
            None => {
                self.done = true;
                return if self.count == self.expected {
                    None
                } else {
                    Some(Err(Error::InvalidInput("entry stream count mismatch")))
                };
            }
        };
        let IndexEntry { key, location } = entry;
        let invalid = key.id == 0
            || key.revision == 0
            || location.commit_sequence == 0
            || location.commit_sequence > self.watermark
            || location.frame_offset < 16
            || location.member >= 1024
            || self.count >= self.expected
            || self.count >= MAX_INDEX_ENTRIES
            || self.previous.is_some_and(|p| {
                key <= p.key
                    || ((key.namespace, key.id) == (p.key.namespace, p.key.id)
                        && (location.commit_sequence <= p.location.commit_sequence
                            || location.frame_offset <= p.location.frame_offset))
            });
        if invalid {
            self.done = true;
            return Some(Err(Error::InvalidInput("invalid ordered entry stream")));
        }
        self.count += 1;
        self.previous = Some(entry);
        Some(Ok(entry))
    }
}
#[derive(Clone, Copy)]
struct SourceStats {
    groups: u64,
    sparse: bool,
}
fn inspect<I: Iterator<Item = Result<IndexEntry>>>(
    source: I,
    count: u64,
    watermark: u64,
) -> Result<SourceStats> {
    bounded_count(count)?;
    let mut stats = SourceStats {
        groups: 0,
        sparse: false,
    };
    let mut previous: Option<IndexKey> = None;
    for entry in Checked::new(source, count, watermark) {
        let key = entry?.key;
        let same = previous.is_some_and(|p| (p.namespace, p.id) == (key.namespace, key.id));
        if !same {
            stats.groups = stats.groups.checked_add(1).ok_or(Error::TooLarge)?;
        }
        let expected = if same {
            previous.and_then(|p| p.revision.checked_add(1))
        } else {
            Some(1)
        };
        stats.sparse |= expected != Some(key.revision);
        previous = Some(key);
    }
    Ok(stats)
}
struct Writer<W> {
    inner: W,
    checksum: crc32fast::Hasher,
    bytes: u64,
}
impl<W: Write> Writer<W> {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let total = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or(Error::TooLarge)?;
        if total > MAX_INDEX_BYTES - FOOTER as u64 {
            return Err(Error::TooLarge);
        }
        self.inner.write_all(bytes)?;
        self.checksum.update(bytes);
        self.bytes = total;
        Ok(())
    }
    fn u64(&mut self, n: u64) -> Result<()> {
        self.bytes(&n.to_le_bytes())
    }
    fn varint(&mut self, mut n: u64) -> Result<()> {
        let mut b = [0; 10];
        let mut i = 0;
        loop {
            b[i] = (n & 127) as u8;
            n >>= 7;
            if n != 0 {
                b[i] |= 128;
            }
            i += 1;
            if n == 0 {
                break;
            }
        }
        self.bytes(&b[..i])
    }
    fn finish(mut self) -> Result<()> {
        self.inner
            .write_all(&self.checksum.finalize().to_le_bytes())?;
        Ok(())
    }
}
#[cfg(test)]
fn encode_entries<I, W: Write>(
    source: I,
    count: u64,
    watermark: u64,
    format: CacheFormat,
    output: W,
) -> Result<()>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    let stats = inspect(source.clone(), count, watermark)?;
    encode_preflighted(source, count, watermark, format, stats, output)
}
fn size_proven(count: u64, stats: SourceStats, format: CacheFormat) -> Result<bool> {
    bounded_count(count)?;
    match format {
        CacheFormat::Legacy => legacy_size(count).map(|_| true),
        CacheFormat::Grouped => grouped_size(count, stats.groups).map(|_| true),
        CacheFormat::Delta => {
            // Each group uses at most three 10-byte varints; each entry has
            // three 10-byte varints plus <=2 bytes for member/deleted flags.
            let upper = stats
                .groups
                .checked_mul(30)
                .and_then(|g| count.checked_mul(32).and_then(|n| g.checked_add(n)))
                .and_then(|n| n.checked_add(44))
                .ok_or(Error::TooLarge)?;
            Ok(upper <= MAX_INDEX_BYTES)
        }
    }
}
fn encode_preflighted<I, W: Write>(
    source: I,
    count: u64,
    watermark: u64,
    format: CacheFormat,
    stats: SourceStats,
    output: W,
) -> Result<()>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    match format {
        CacheFormat::Legacy => {
            legacy_size(count)?;
        }
        CacheFormat::Grouped => {
            grouped_size(count, stats.groups)?;
        }
        CacheFormat::Delta => (),
    }
    let mut w = Writer {
        inner: output,
        checksum: crc32fast::Hasher::new(),
        bytes: 0,
    };
    let mut header = [0; HEADER];
    header[..8].copy_from_slice(MAGIC);
    let version = if format == CacheFormat::Legacy {
        if stats.sparse { 2u16 } else { 1u16 }
    } else {
        3u16
    };
    header[8..10].copy_from_slice(&version.to_le_bytes());
    header[10] = match format {
        CacheFormat::Legacy => 0,
        CacheFormat::Grouped => 1,
        CacheFormat::Delta => 2,
    };
    header[16..24].copy_from_slice(&watermark.to_le_bytes());
    header[24..32].copy_from_slice(&count.to_le_bytes());
    w.bytes(&header)?;
    let mut iter = Checked::new(source, count, watermark);
    if format == CacheFormat::Legacy {
        let mut previous_key: Option<IndexKey> = None;
        for entry in iter {
            let IndexEntry { key, location } = entry?;
            let expected = previous_key
                .filter(|p| (p.namespace, p.id) == (key.namespace, key.id))
                .map_or(Some(1), |p| p.revision.checked_add(1));
            if version == 1 && expected != Some(key.revision) {
                return Err(Error::InvalidInput("source sparsity changed"));
            }
            previous_key = Some(key);
            let mut b = [0; ENTRY];
            b[..8].copy_from_slice(&key.namespace.to_le_bytes());
            b[8..16].copy_from_slice(&key.id.to_le_bytes());
            b[16..24].copy_from_slice(&key.revision.to_le_bytes());
            b[24..32].copy_from_slice(&location.commit_sequence.to_le_bytes());
            b[32..40].copy_from_slice(&location.frame_offset.to_le_bytes());
            b[40..44].copy_from_slice(&location.member.to_le_bytes());
            b[44] = u8::from(location.deleted);
            w.bytes(&b)?;
        }
    } else {
        w.u64(stats.groups)?;
        let mut identity = (0, 0);
        let mut written_groups = 0u64;
        while let Some(first) = iter.next() {
            let first = first?;
            written_groups = written_groups.checked_add(1).ok_or(Error::TooLarge)?;
            let current = (first.key.namespace, first.key.id);
            let mut group_count = 1u64;
            for entry in iter.clone() {
                let entry = entry?;
                if (entry.key.namespace, entry.key.id) != current {
                    break;
                }
                group_count = group_count.checked_add(1).ok_or(Error::TooLarge)?;
            }
            if format == CacheFormat::Grouped {
                w.u64(current.0)?;
                w.u64(current.1)?;
                w.u64(group_count)?;
            } else {
                w.varint(current.0.checked_sub(identity.0).ok_or(Error::Conflict)?)?;
                w.varint(if current.0 == identity.0 {
                    current.1.checked_sub(identity.1).ok_or(Error::Conflict)?
                } else {
                    current.1
                })?;
                w.varint(group_count)?;
            }
            let mut first = Some(first);
            let mut previous: Option<IndexEntry> = None;
            for _ in 0..group_count {
                let entry = match first.take() {
                    Some(e) => e,
                    None => iter
                        .next()
                        .ok_or(Error::InvalidInput("source group changed"))??,
                };
                let IndexEntry { key, location } = entry;
                if (key.namespace, key.id) != current {
                    return Err(Error::InvalidInput("source group changed"));
                }
                if format == CacheFormat::Grouped {
                    let mut b = [0; 32];
                    b[..8].copy_from_slice(&key.revision.to_le_bytes());
                    b[8..16].copy_from_slice(&location.commit_sequence.to_le_bytes());
                    b[16..24].copy_from_slice(&location.frame_offset.to_le_bytes());
                    b[24..28].copy_from_slice(&location.member.to_le_bytes());
                    b[28] = u8::from(location.deleted);
                    w.bytes(&b)?;
                } else {
                    w.varint(if let Some(p) = previous {
                        key.revision
                            .checked_sub(p.key.revision)
                            .ok_or(Error::Conflict)?
                    } else {
                        key.revision
                    })?;
                    w.varint(if let Some(p) = previous {
                        location
                            .commit_sequence
                            .checked_sub(p.location.commit_sequence)
                            .ok_or(Error::Conflict)?
                    } else {
                        location.commit_sequence
                    })?;
                    w.varint(if let Some(p) = previous {
                        location
                            .frame_offset
                            .checked_sub(p.location.frame_offset)
                            .ok_or(Error::Conflict)?
                    } else {
                        location.frame_offset
                    })?;
                    w.varint((u64::from(location.member) << 1) | u64::from(location.deleted))?;
                }
                previous = Some(entry);
            }
            identity = current;
        }
        if written_groups != stats.groups {
            return Err(Error::InvalidInput("source groups changed"));
        }
    }
    w.finish()
}
#[cfg(test)]
fn encode<W: Write>(
    index: &PrimaryIndex,
    watermark: u64,
    format: CacheFormat,
    output: W,
) -> Result<()> {
    encode_entries(
        entries(index),
        index.len() as u64,
        watermark,
        format,
        output,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    BeforeWrite,
    BeforeFlush,
    BeforeSync,
    BeforeRename,
    AfterPublish,
    AfterParentSync,
}
fn temporary(path: &Path) -> Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or(Error::InvalidInput("cache filename required"))?
        .to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}
fn publish_entries<I, F: FnMut(Stage) -> Result<()>>(
    source: I,
    count: u64,
    path: &Path,
    watermark: u64,
    format: CacheFormat,
    mut stage: F,
) -> Result<()>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    let stats = inspect(source.clone(), count, watermark)?;
    // Exact fixed-format sizes and a conservative Delta upper bound usually
    // prove fit. Larger Delta bounds still get exact dry encoding: a pessimistic
    // estimate must not reject a compact stream that actually fits.
    if !size_proven(count, stats, format)? {
        encode_preflighted(
            source.clone(),
            count,
            watermark,
            format,
            stats,
            std::io::sink(),
        )?;
    }
    stage(Stage::BeforeWrite)?;
    let tmp = temporary(path)?;
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    let mut out = BufWriter::new(file);
    if let Err(error) = encode_preflighted(source, count, watermark, format, stats, &mut out) {
        drop(out);
        // This is only our unpublished temporary; keep the prior cache intact.
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    stage(Stage::BeforeFlush)?;
    out.flush()?;
    stage(Stage::BeforeSync)?;
    out.get_ref().sync_all()?;
    drop(out);
    stage(Stage::BeforeRename)?;
    fs::rename(&tmp, path)?;
    stage(Stage::AfterPublish)?;
    sync_parent(path)?;
    stage(Stage::AfterParentSync)?;
    Ok(())
}
fn publish<F: FnMut(Stage) -> Result<()>>(
    index: &PrimaryIndex,
    path: &Path,
    watermark: u64,
    format: CacheFormat,
    stage: F,
) -> Result<()> {
    publish_entries(
        entries(index),
        index.len() as u64,
        path,
        watermark,
        format,
        stage,
    )
}
pub(super) fn save_entries<I>(
    path: &Path,
    watermark: u64,
    count: u64,
    format: CacheFormat,
    source: I,
) -> Result<()>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    publish_entries(source, count, path, watermark, format, |_| Ok(()))
}
pub(super) fn save(
    index: &PrimaryIndex,
    path: &Path,
    watermark: u64,
    format: CacheFormat,
) -> Result<()> {
    publish(index, path, watermark, format, |_| Ok(()))
}
#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<()> {
    let p = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(p)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(revisions: u64) -> PrimaryIndex {
        let mut i = PrimaryIndex::new();
        for revision in 1..=revisions {
            i.insert(IndexEntry {
                key: IndexKey {
                    namespace: 0,
                    id: 1,
                    revision,
                },
                location: Location {
                    commit_sequence: revision,
                    frame_offset: revision * 16,
                    member: 0,
                    deleted: revision == 2,
                },
            })
            .unwrap();
        }
        i
    }
    #[test]
    fn physical_and_logical_limits_stop_before_buffer_write_or_publication() {
        assert_eq!(MAX_INDEX_ENTRIES, 5_592_404);
        assert!(matches!(
            legacy_size(MAX_INDEX_ENTRIES + 1),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            grouped_size(MAX_INDEX_ENTRIES, MAX_INDEX_ENTRIES),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            grouped_size(u64::MAX, u64::MAX),
            Err(Error::TooLarge)
        ));
        let mut writer = Writer {
            inner: Vec::new(),
            checksum: crc32fast::Hasher::new(),
            bytes: MAX_INDEX_BYTES - 4,
        };
        assert!(matches!(writer.bytes(&[1]), Err(Error::TooLarge)));
        assert!(writer.inner.is_empty());
    }
    struct FailAfter {
        left: usize,
        written: usize,
    }
    impl Write for FailAfter {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.left == 0 {
                return Err(std::io::Error::other("injected write failure"));
            }
            let n = b.len().min(self.left);
            self.left -= n;
            self.written += n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn encoder_propagates_every_short_write_and_footer_failure() {
        for format in [
            CacheFormat::Legacy,
            CacheFormat::Grouped,
            CacheFormat::Delta,
        ] {
            let i = fixture(2);
            let mut full = Vec::new();
            encode(&i, 2, format, &mut full).unwrap();
            for n in 0..full.len() {
                let mut out = FailAfter {
                    left: n,
                    written: 0,
                };
                assert!(encode(&i, 2, format, &mut out).is_err(), "cut{n}");
                assert_eq!(out.written, n);
            }
        }
    }
    #[test]
    fn publication_errors_preserve_old_before_rename_and_valid_new_after_publish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("primary.cbi");
        for format in [
            CacheFormat::Legacy,
            CacheFormat::Grouped,
            CacheFormat::Delta,
        ] {
            for point in [
                Stage::BeforeWrite,
                Stage::BeforeFlush,
                Stage::BeforeSync,
                Stage::BeforeRename,
                Stage::AfterPublish,
                Stage::AfterParentSync,
            ] {
                fixture(1).save_with_format(&path, 1, format).unwrap();
                let old = fs::read(&path).unwrap();
                let result = publish(&fixture(2), &path, 2, format, |stage| {
                    if stage == point {
                        Err(Error::Io(std::io::Error::other(
                            "injected publication failure",
                        )))
                    } else {
                        Ok(())
                    }
                });
                assert!(result.is_err());
                if matches!(point, Stage::AfterPublish | Stage::AfterParentSync) {
                    assert_eq!(load(&path, 2).unwrap(), fixture(2));
                    assert_eq!(
                        validate(&path, 2, &fixture(2)).unwrap(),
                        CacheValidation::Match
                    );
                } else {
                    assert_eq!(fs::read(&path).unwrap(), old);
                    assert_eq!(load(&path, 1).unwrap(), fixture(1));
                }
            }
        }
    }
    #[test]
    #[ignore = "invoked by parent crash-publication test only"]
    fn publication_child() {
        let path =
            std::env::var_os("CBR_INDEX_CRASH_PATH").expect("parent supplies isolated cache path");
        let point = std::env::var("CBR_INDEX_CRASH_STAGE").unwrap();
        let format = match std::env::var("CBR_INDEX_CRASH_FORMAT").unwrap().as_str() {
            "legacy" => CacheFormat::Legacy,
            "grouped" => CacheFormat::Grouped,
            _ => CacheFormat::Delta,
        };
        publish(&fixture(2), Path::new(&path), 2, format, |stage| {
            if format!("{stage:?}") == point {
                std::process::exit(77);
            }
            Ok(())
        })
        .unwrap();
        panic!("child did not reach injected exit");
    }
    #[test]
    fn child_exit_at_publication_stages_keeps_complete_old_or_new_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("primary.cbi");
        for (format, name) in [
            (CacheFormat::Legacy, "legacy"),
            (CacheFormat::Grouped, "grouped"),
            (CacheFormat::Delta, "delta"),
        ] {
            for point in [
                Stage::BeforeWrite,
                Stage::BeforeFlush,
                Stage::BeforeSync,
                Stage::BeforeRename,
                Stage::AfterPublish,
                Stage::AfterParentSync,
            ] {
                fixture(1).save_with_format(&path, 1, format).unwrap();
                let status = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "cache::tests::publication_child", "--ignored"])
                    .env("CBR_INDEX_CRASH_PATH", &path)
                    .env("CBR_INDEX_CRASH_STAGE", format!("{point:?}"))
                    .env("CBR_INDEX_CRASH_FORMAT", name)
                    .status()
                    .unwrap();
                assert_eq!(status.code(), Some(77));
                let expected = if matches!(point, Stage::AfterPublish | Stage::AfterParentSync) {
                    2
                } else {
                    1
                };
                assert_eq!(load(&path, expected).unwrap(), fixture(expected));
            }
        }
    }
    #[test]
    fn delta_size_bound_is_conservative_and_does_not_reject_large_compressible_sources() {
        let n = (MAX_INDEX_BYTES - 44) / 62;
        let stats = |groups| SourceStats {
            groups,
            sparse: false,
        };
        assert!(size_proven(n, stats(n), CacheFormat::Delta).unwrap());
        // Crossing the pessimistic bound requests exact dry encoding, not TooLarge.
        assert!(!size_proven(n + 1, stats(n + 1), CacheFormat::Delta).unwrap());
        assert!(size_proven(MAX_INDEX_ENTRIES, stats(1), CacheFormat::Delta).unwrap());
        assert!(size_proven(MAX_INDEX_ENTRIES, stats(1), CacheFormat::Legacy).unwrap());
        assert!(size_proven(1, stats(1), CacheFormat::Grouped).unwrap());
        assert!(matches!(
            size_proven(1, stats(u64::MAX), CacheFormat::Delta),
            Err(Error::TooLarge)
        ));
        assert!(matches!(
            size_proven(MAX_INDEX_ENTRIES + 1, stats(1), CacheFormat::Legacy),
            Err(Error::TooLarge)
        ));
    }
}
