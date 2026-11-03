//! Reconstructible primary/history index. Callers must compare cache contents with
//! authoritative replay; a valid checksum alone never makes an index authoritative.
use canonical_core::{Error, Result};
use std::{collections::BTreeMap, path::Path};
mod cache;

pub const MAX_INDEX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_INDEX_ENTRIES: u64 = (MAX_INDEX_BYTES - 36) / 48;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheFormat {
    Legacy,
    Grouped,
    Delta,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheValidation {
    Match,
    ContentMismatch,
}
/// Compare a cache with a sorted repeatable fallible canonical entry stream.
/// Clones must reproduce the same entries without sharing iteration position. The
/// reader opens first. If its bounded entry count differs from `expected_count`,
/// the entire cache is verified and ContentMismatch returned without inspecting
/// the source: cache bounds never constrain authoritative journal cardinality.
/// Equal counts require exact source count, strict key order and physical bounds;
/// invalid source entries are errors. Mismatches still verify the complete CRC.
/// The standalone `PrimaryIndex::validate_cache` keeps its earlier semantics.
pub fn validate_cache_entries<I>(
    path: &Path,
    watermark: u64,
    expected_count: u64,
    entries: I,
) -> Result<CacheValidation>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    cache::validate_entries(path, watermark, expected_count, entries)
}
/// Publish a cache from a sorted repeatable fallible stream, without materializing an index.
/// Source clones must be repeatable and inexpensive. Source inspection and bounded
/// size proof precede temporary-file creation; Delta uses exact dry encoding only
/// when its conservative bound cannot prove fit. Actual encoding revalidates every
/// entry before publication and cleans its unpublished temporary on source errors. Sparse
/// revisions select Legacy v2; contiguous streams preserve the published v1 bytes.
pub fn save_entries_with_format<I>(
    path: &Path,
    watermark: u64,
    expected_count: u64,
    format: CacheFormat,
    entries: I,
) -> Result<()>
where
    I: Iterator<Item = Result<IndexEntry>> + Clone,
{
    cache::save_entries(path, watermark, expected_count, format, entries)
}
const HEADER: usize = 32;
const ENTRY: usize = 48;
const FOOTER: usize = 4;
const MAGIC: &[u8; 8] = b"CBRIDX01";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexKey {
    pub namespace: u64,
    pub id: u64,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub commit_sequence: u64,
    pub frame_offset: u64,
    pub member: u32,
    pub deleted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub key: IndexKey,
    pub location: Location,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrimaryIndex {
    history: BTreeMap<IndexKey, Location>,
    latest: BTreeMap<(u64, u64), u64>,
}
impl PrimaryIndex {
    /// Write an explicitly selected reconstructible cache representation.
    pub fn save_with_format(&self, path: &Path, watermark: u64, format: CacheFormat) -> Result<()> {
        match format {
            CacheFormat::Legacy => self.save(path, watermark),
            _ => cache::save(self, path, watermark, format),
        }
    }

    /// Validate a cache against authoritative replay without building a second index.
    pub fn validate_cache(path: &Path, watermark: u64, expected: &Self) -> Result<CacheValidation> {
        cache::validate(path, watermark, expected)
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Insert revisions in order for each identity. Different identities may interleave.
    pub fn insert(&mut self, entry: IndexEntry) -> Result<()> {
        self.insert_inner(entry, false)
    }
    /// Insert an available revision after explicit retention; authoritative replay must validate gaps.
    pub fn insert_retained(&mut self, entry: IndexEntry) -> Result<()> {
        self.insert_inner(entry, true)
    }
    fn insert_inner(&mut self, entry: IndexEntry, sparse: bool) -> Result<()> {
        let IndexEntry { key, location } = entry;
        if key.id == 0
            || key.revision == 0
            || location.commit_sequence == 0
            || location.frame_offset < 16
            || location.member >= 1024
        {
            return Err(Error::InvalidInput("invalid primary index entry"));
        }
        if self.history.contains_key(&key) {
            return Err(Error::Conflict);
        }
        let identity = (key.namespace, key.id);
        if let Some(revision) = self.latest.get(&identity) {
            if key.revision <= *revision
                || (!sparse && revision.checked_add(1) != Some(key.revision))
            {
                return Err(Error::Conflict);
            }
            let prior = self.history[&IndexKey {
                namespace: key.namespace,
                id: key.id,
                revision: *revision,
            }];
            if location.commit_sequence <= prior.commit_sequence
                || location.frame_offset <= prior.frame_offset
            {
                return Err(Error::Conflict);
            }
        } else if !sparse && key.revision != 1 {
            return Err(Error::Conflict);
        }
        self.history.insert(key, location);
        self.latest.insert(identity, key.revision);
        Ok(())
    }
    /// None selects latest revision, including its tombstone; caller handles visibility.
    pub fn get(&self, namespace: u64, id: u64, revision: Option<u64>) -> Option<Location> {
        let revision = revision.or_else(|| self.latest.get(&(namespace, id)).copied())?;
        self.history
            .get(&IndexKey {
                namespace,
                id,
                revision,
            })
            .copied()
    }
    pub fn entries(&self) -> Vec<IndexEntry> {
        self.history
            .iter()
            .map(|(key, location)| IndexEntry {
                key: *key,
                location: *location,
            })
            .collect()
    }
    pub fn len(&self) -> usize {
        self.history.len()
    }
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// Save the published v1/v2 representation, preserving its exact bytes.
    /// Requires one logical writer; only the derived cache is replaced.
    pub fn save(&self, path: &Path, watermark: u64) -> Result<()> {
        cache::save(self, path, watermark, CacheFormat::Legacy)
    }

    /// Decode and validate a bounded v1/v2/v3 cache; replay must still prove its contents.
    pub fn load(path: &Path, expected_watermark: u64) -> Result<Self> {
        cache::load(path, expected_watermark)
    }
}
