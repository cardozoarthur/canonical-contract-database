//! A single cooperative writer with durable acknowledgments and immutable reader snapshots.
//! Strict open never changes a segment; explicit recovery backs up incomplete tails.
use canonical_codec::{FRAME_HEADER, FRAME_OVERHEAD, decode_frame, decode_frame_ref, encode_frame};
use canonical_core::{Error, MAX_FRAME_BYTES, Result};
use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(feature = "memory-profile")]
mod memory;

const MANIFEST: &[u8; 16] = b"CBRMAN01\x01\x00\x01\x00\x00\x00\x00\x00";
const MAX_GENERATION: u64 = 99_999_999;
const MANIFEST_V2_MAGIC: &[u8; 8] = b"CBRMAN02";

const SEGMENT: &[u8; 16] = b"CBRSEG01\x01\x00\x01\x00\x00\x00\x00\x00";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub sequence: u64,
    pub payload: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub sequence: u64,
}

/// Ordered durable-write observation points for deterministic fault injection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendStage {
    Prepared,
    HeaderWritten,
    BodyWritten,
    BeforeSync,
    Synced,
    Published,
}

/// Boundaries in the generation publication protocol. Hook failure requires reopen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RewriteStage {
    Prepared,
    SegmentWritten,
    SegmentSynced,
    ManifestWritten,
    ManifestPublished,
    Activated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationReport {
    pub previous_generation: u64,
    pub generation: u64,
    pub previous_active_bytes: u64,
    pub active_bytes: u64,
    /// All recognized generation files, including retained backups and orphan generations.
    pub total_segment_bytes: u64,
    pub frames: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub removed_generations: Vec<u64>,
    pub removed_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub discarded_bytes: u64,
    pub backup: Option<PathBuf>,
    /// None: no checkpoint. Some(false): invalid/stale checkpoint ignored.
    pub checkpoint_valid: Option<bool>,
}

/// Physical frame activity. Appending does not count as a replay or point read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageStats {
    pub replay_frames: u64,
    pub point_reads: u64,
    pub point_bytes: u64,
}

struct Replay {
    entries: Vec<Arc<Entry>>,
    offsets: Vec<u64>,
}

pub struct Store {
    path: PathBuf,
    _lock: File,
    segment: File,
    generation: u64,
    entries: Vec<Arc<Entry>>,
    frame_offsets: Vec<u64>,
    stats: StorageStats,
    poisoned: bool,
}

fn open_rw(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}
fn validate_header(file: &mut File, expected: &[u8; 16]) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0; 16];
    file.read_exact(&mut header).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::IncompleteTail { offset: 0 }
        } else {
            e.into()
        }
    })?;
    if header[..8] != expected[..8] {
        return Err(Error::Corruption { offset: 0 });
    }
    let version = u16::from_le_bytes(header[8..10].try_into().unwrap());
    if version != 1 {
        return Err(Error::UnsupportedVersion(version));
    }
    if &header != expected {
        return Err(Error::Corruption { offset: 0 });
    }
    Ok(())
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_mode(path.as_ref(), false).map(|(store, _)| store)
    }

    /// Explicitly repair only a final incomplete frame, after syncing a full backup.
    /// Complete corrupt frames and incomplete segment headers are never repaired.
    pub fn recover(path: impl AsRef<Path>) -> Result<(Self, RecoveryReport)> {
        Self::open_mode(path.as_ref(), true)
    }

    fn open_mode(path: &Path, recover: bool) -> Result<(Self, RecoveryReport)> {
        #[cfg(feature = "memory-profile")]
        let _origin = canonical_memory::scope(canonical_memory::Component::StoreTemporary);
        fs::create_dir_all(path)?;
        let lock = open_rw(&path.join("writer.lock"))?;
        FileExt::try_lock_exclusive(&lock).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                Error::WriterBusy
            } else {
                e.into()
            }
        })?;
        let manifest_path = path.join("manifest.cbr");
        let segment_path = generation_path(path, 1);
        if !manifest_path.exists() {
            // A nonempty uncommitted initialization is not silently replaced.
            if path.join("segments").exists()
                && fs::read_dir(path.join("segments"))?.next().is_some()
            {
                return Err(Error::InvalidInput(
                    "orphan segment; preserve and diagnose initialization",
                ));
            }
            fs::create_dir_all(path.join("segments"))?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&segment_path)?;
            file.write_all(SEGMENT)?;
            file.sync_all()?;
            sync_dir(&path.join("segments"))?;
            // Publish the manifest only after a complete, synced segment header.
            let temp = path.join("manifest.init");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(MANIFEST)?;
            file.sync_all()?;
            drop(file);
            fs::rename(temp, &manifest_path)?;
            sync_dir(path)?;
        }
        let generation = read_manifest(&manifest_path)?;
        reject_symlink(&path.join("segments"))?;
        let segment_path = generation_path(path, generation);
        reject_symlink(&segment_path)?;
        let mut segment = OpenOptions::new()
            .read(true)
            .write(true)
            .open(segment_path)?;
        validate_header(&mut segment, SEGMENT)?;
        let mut report = RecoveryReport::default();
        let mut stats = StorageStats::default();
        let replayed = match replay(&mut segment, &mut stats) {
            Ok(replayed) => replayed,
            Err(Error::IncompleteTail { offset }) if recover && offset >= 16 => {
                let end = segment.metadata()?.len();
                let backup = backup_segment(path, &mut segment)?;
                segment.set_len(offset)?;
                segment.sync_all()?;
                segment.seek(SeekFrom::Start(16))?;
                let replayed = replay(&mut segment, &mut stats)?;
                report.discarded_bytes = end - offset;
                report.backup = Some(backup);
                replayed
            }
            Err(error) => return Err(error),
        };
        report.checkpoint_valid =
            validate_checkpoint(path, &mut segment, &replayed.entries, generation)?;
        let store = Self {
            path: {
                #[cfg(feature = "memory-profile")]
                let _c = canonical_memory::scope(canonical_memory::Component::StoreContainers);
                path.to_path_buf()
            },
            _lock: lock,
            segment,
            generation,
            entries: replayed.entries,
            frame_offsets: replayed.offsets,
            stats,
            poisoned: false,
        };
        #[cfg(feature = "memory-profile")]
        memory::publish_store(&store);
        Ok((store, report))
    }

    /// Success means the entire frame has passed sync_all. I/O failure poisons the writer.
    pub fn append(&mut self, payload: &[u8]) -> Result<Receipt> {
        self.append_with_hook(payload, |_| Ok(()))
    }

    pub fn ensure_healthy(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::Poisoned)
        } else {
            Ok(())
        }
    }

    /// A hook error has the same indeterminate outcome as an I/O error: reopen first.
    pub fn append_with_hook<F>(&mut self, payload: &[u8], mut hook: F) -> Result<Receipt>
    where
        F: FnMut(AppendStage) -> Result<()>,
    {
        #[cfg(feature = "memory-profile")]
        let _origin = canonical_memory::scope(canonical_memory::Component::StoreTemporary);
        self.ensure_healthy()?;
        let sequence = (self.entries.len() as u64)
            .checked_add(1)
            .ok_or(Error::InvalidInput("sequence exhausted"))?;
        let frame = encode_frame(sequence, payload)?;
        let write_result = (|| -> Result<()> {
            hook(AppendStage::Prepared)?;
            let offset = self.segment.seek(SeekFrom::End(0))?;
            self.segment.write_all(&frame[..FRAME_HEADER])?;
            hook(AppendStage::HeaderWritten)?;
            self.segment.write_all(&frame[FRAME_HEADER..])?;
            hook(AppendStage::BodyWritten)?;
            hook(AppendStage::BeforeSync)?;
            self.segment.sync_all()?;
            hook(AppendStage::Synced)?;
            let payload = {
                #[cfg(feature = "memory-profile")]
                let _c = canonical_memory::scope(canonical_memory::Component::StorePayload);
                payload.to_vec()
            };
            {
                #[cfg(feature = "memory-profile")]
                let _c = canonical_memory::scope(canonical_memory::Component::StoreContainers);
                self.entries.push(Arc::new(Entry { sequence, payload }));
                self.frame_offsets.push(offset);
            }

            hook(AppendStage::Published)
        })();
        if let Err(error) = write_result {
            self.poisoned = true;
            return Err(error);
        }
        Ok(Receipt { sequence })
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn active_segment_path(&self) -> PathBuf {
        generation_path(&self.path, self.generation)
    }

    /// Rewrite one payload per original commit, retaining its original sequence.
    /// Older generations remain on disk until explicit cleanup. No space gain is implied.
    pub fn rewrite(&mut self, payloads: &[Vec<u8>]) -> Result<GenerationReport> {
        self.rewrite_with_hook(payloads, |_| Ok(()))
    }

    /// Publish a synced generation via a synced, checksummed manifest and atomic rename.
    /// Any hook or I/O failure poisons writes; reopen selects the authoritative generation.
    /// A process exit at any boundary leaves the complete old or complete new generation.
    /// Directory flush is unavailable through portable std on Windows: no power-loss claim.
    pub fn rewrite_with_hook<F>(
        &mut self,
        payloads: &[Vec<u8>],
        mut hook: F,
    ) -> Result<GenerationReport>
    where
        F: FnMut(RewriteStage) -> Result<()>,
    {
        self.ensure_healthy()?;
        if payloads.len() != self.entries.len() {
            return Err(Error::InvalidInput("rewrite must preserve commit count"));
        }
        // Check all caller lengths before touching disk. Each frame is allocated individually.
        if payloads
            .iter()
            .any(|payload| payload.len() > MAX_FRAME_BYTES - FRAME_OVERHEAD)
        {
            return Err(Error::TooLarge);
        }
        let result = (|| -> Result<GenerationReport> {
            self.verify_active()?;
            hook(RewriteStage::Prepared)?;
            let previous_generation = self.generation;
            let previous_active_bytes = self.segment.metadata()?.len();
            let (generation, mut segment) = self.create_generation()?;
            segment.write_all(SEGMENT)?;
            let mut offsets = Vec::with_capacity(payloads.len());
            let mut entries = Vec::with_capacity(payloads.len());
            let mut offset = 16u64;
            for (index, payload) in payloads.iter().enumerate() {
                let sequence = index as u64 + 1;
                let frame = encode_frame(sequence, payload)?;
                segment.write_all(&frame)?;
                offsets.push(offset);
                offset = offset
                    .checked_add(frame.len() as u64)
                    .ok_or(Error::TooLarge)?;
                entries.push(Arc::new(Entry {
                    sequence,
                    payload: payload.clone(),
                }));
            }
            hook(RewriteStage::SegmentWritten)?;
            segment.sync_all()?;
            sync_dir(&self.path.join("segments"))?;
            hook(RewriteStage::SegmentSynced)?;
            // Unique temporary names preserve evidence from earlier interrupted attempts.
            let temporary = self.path.join(format!("manifest-{generation:08}.tmp"));
            let mut manifest = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            manifest.write_all(&encode_manifest(generation))?;
            manifest.sync_all()?;
            drop(manifest);
            hook(RewriteStage::ManifestWritten)?;
            fs::rename(&temporary, self.path.join("manifest.cbr"))?;
            sync_dir(&self.path)?;
            hook(RewriteStage::ManifestPublished)?;
            self.segment = segment;
            self.generation = generation;
            self.entries = entries;
            self.frame_offsets = offsets;
            hook(RewriteStage::Activated)?;
            Ok(GenerationReport {
                previous_generation,
                generation,
                previous_active_bytes,
                active_bytes: offset,
                total_segment_bytes: self.total_segment_bytes()?,
                frames: payloads.len() as u64,
            })
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Delete only recognized older generation files after verifying current disk state.
    /// Unknown files and future orphan generations are retained for diagnosis. A failed
    /// cleanup may have removed some obsolete backups, but never the active generation.
    pub fn cleanup_obsolete_generations(&mut self) -> Result<CleanupReport> {
        self.ensure_healthy()?;
        let result = (|| -> Result<CleanupReport> {
            self.verify_active()?;
            let mut report = CleanupReport::default();
            for entry in fs::read_dir(self.path.join("segments"))? {
                let entry = entry?;
                let Some(generation) = parse_generation(&entry.file_name()) else {
                    continue;
                };
                if generation >= self.generation {
                    continue;
                }
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    continue;
                }
                // Preserve malformed backups as diagnostic evidence, rather than deleting them.
                let mut backup = File::open(entry.path())?;
                if validate_header(&mut backup, SEGMENT).is_err()
                    || replay(&mut backup, &mut StorageStats::default()).is_err()
                {
                    continue;
                }
                drop(backup);
                fs::remove_file(entry.path())?;
                report.removed_generations.push(generation);
                report.removed_bytes = report
                    .removed_bytes
                    .checked_add(metadata.len())
                    .ok_or(Error::TooLarge)?;
            }
            report.removed_generations.sort_unstable();
            sync_dir(&self.path.join("segments"))?;
            Ok(report)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn verify_active(&mut self) -> Result<()> {
        if read_manifest(&self.path.join("manifest.cbr"))? != self.generation {
            return Err(Error::Corruption { offset: 0 });
        }
        reject_symlink(&self.path.join("segments"))?;
        reject_symlink(&self.active_segment_path())?;
        // Open by authoritative path too, detecting replacements behind an already open handle.
        let mut active = File::open(self.active_segment_path())?;
        validate_header(&mut active, SEGMENT)?;
        let replayed = replay(&mut active, &mut self.stats)?;
        if replayed.entries != self.entries || replayed.offsets != self.frame_offsets {
            return Err(Error::Corruption { offset: 0 });
        }
        Ok(())
    }

    fn create_generation(&self) -> Result<(u64, File)> {
        reject_symlink(&self.path.join("segments"))?;
        let mut generation = self.generation;
        // Bound attempts in directories populated with interrupted generations.
        for _ in 0..10_000 {
            generation = generation
                .checked_add(1)
                .ok_or(Error::InvalidInput("generation exhausted"))?;
            if generation > MAX_GENERATION {
                return Err(Error::InvalidInput("generation exhausted"));
            }
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(generation_path(&self.path, generation))
            {
                Ok(file) => return Ok((generation, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::InvalidInput("orphan generation limit reached"))
    }

    pub fn total_segment_bytes(&self) -> Result<u64> {
        reject_symlink(&self.path.join("segments"))?;
        let mut bytes = 0u64;
        for entry in fs::read_dir(self.path.join("segments"))? {
            let entry = entry?;
            if parse_generation(&entry.file_name()).is_none() {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                bytes = bytes.checked_add(metadata.len()).ok_or(Error::TooLarge)?;
            }
        }
        Ok(bytes)
    }

    /// Derived checkpoint, validated on reopen but not used to skip authoritative replay.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        let result = (|| -> Result<()> {
            let end = self.segment.metadata()?.len();
            let checksum = prefix_crc(&mut self.segment, end)?;
            let mut bytes = Vec::with_capacity(48);
            let version = if self.generation == 1 { 1u16 } else { 2u16 };
            bytes.extend_from_slice(if version == 1 {
                b"CBRCHK01"
            } else {
                b"CBRCHK02"
            });
            bytes.extend_from_slice(&version.to_le_bytes());
            bytes.extend_from_slice(&[0; 6]);
            bytes.extend_from_slice(&end.to_le_bytes());
            bytes.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
            if version == 2 {
                bytes.extend_from_slice(&self.generation.to_le_bytes());
            }
            bytes.extend_from_slice(&checksum.to_le_bytes());
            bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
            let temporary = self.path.join("checkpoint.tmp");
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(temporary, self.path.join("checkpoint.cbr"))?;
            sync_dir(&self.path)
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Immutable prefix snapshot; payloads are shared, not copied. Snapshot list is O(records).
    pub fn snapshot(&self) -> Vec<Arc<Entry>> {
        #[cfg(feature = "memory-profile")]
        let _origin = canonical_memory::scope(canonical_memory::Component::StoreSnapshot);
        self.entries.clone()
    }

    /// O(1) shared handle from the current immutable in-memory prefix, without disk IO.
    /// Like snapshot(), this remains available when poisoned. Retained handles keep the
    /// captured prefix alive even if a later generation is activated.
    /// This is not a physical integrity proof; use read_verified_frame_at for that.
    pub fn prefix_entry(&self, sequence: u64) -> Option<Arc<Entry>> {
        let index = usize::try_from(sequence.checked_sub(1)?).ok()?;
        self.entries
            .get(index)
            .filter(|entry| entry.sequence == sequence)
            .map(Arc::clone)
    }

    pub fn io_stats(&self) -> StorageStats {
        self.stats
    }

    /// O(1) lookup in offsets captured during authoritative replay or durable append.
    pub fn frame_offset(&self, sequence: u64) -> Option<u64> {
        let index = usize::try_from(sequence.checked_sub(1)?).ok()?;
        self.frame_offsets.get(index).copied()
    }

    /// Physically verify the complete frame against its immutable replay mirror.
    /// Returning a shared entry never bypasses this call's disk read or integrity checks.
    pub fn read_verified_frame_at(&mut self, offset: u64) -> Result<Arc<Entry>> {
        self.ensure_healthy()?;
        let index = self
            .frame_offsets
            .binary_search(&offset)
            .map_err(|_| Error::InvalidInput("offset is not a known frame boundary"))?;
        self.stats.point_reads = self.stats.point_reads.saturating_add(1);
        let expected = Arc::clone(&self.entries[index]);
        let result = self.read_known_frame(offset, index as u64 + 1, &expected.payload);
        if let Err(error) = result {
            self.poisoned = true;
            return Err(error);
        }
        Ok(expected)
    }

    /// Read a known frame as an owned Entry; the verified mirror proof is identical.
    pub fn read_frame_at(&mut self, offset: u64) -> Result<Entry> {
        self.read_verified_frame_at(offset)
            .map(|entry| (*entry).clone())
    }

    fn read_known_frame(
        &mut self,
        offset: u64,
        expected_sequence: u64,
        expected_payload: &[u8],
    ) -> Result<()> {
        #[cfg(feature = "memory-profile")]
        let _origin = canonical_memory::scope(canonical_memory::Component::StoreTemporary);
        self.segment.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; FRAME_HEADER];
        read_point_bytes(&mut self.segment, &mut self.stats, &mut header, offset)?;
        match decode_frame_ref(&header, offset) {
            Err(Error::IncompleteTail { .. }) => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(Error::Corruption { offset }),
        }
        let sequence = u64::from_le_bytes(header[12..20].try_into().unwrap());
        if sequence != expected_sequence {
            return Err(Error::Corruption { offset });
        }
        let len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if len != expected_payload.len() {
            return Err(Error::Corruption { offset });
        }
        let total = len.checked_add(FRAME_OVERHEAD).ok_or(Error::TooLarge)?;
        if total > MAX_FRAME_BYTES {
            return Err(Error::TooLarge);
        }
        let mut frame = Vec::with_capacity(total);
        frame.extend_from_slice(&header);
        frame.resize(total, 0);
        read_point_bytes(
            &mut self.segment,
            &mut self.stats,
            &mut frame[FRAME_HEADER..],
            offset,
        )?;
        let (_, payload, _) = decode_frame_ref(&frame, offset)?;
        if payload != expected_payload {
            return Err(Error::Corruption { offset });
        }
        Ok(())
    }
}

fn generation_path(path: &Path, generation: u64) -> PathBuf {
    path.join("segments").join(format!("{generation:08}.cbr"))
}

fn parse_generation(name: &std::ffi::OsStr) -> Option<u64> {
    let name = name.to_str()?;
    if name.len() != 12
        || !name.ends_with(".cbr")
        || !name.as_bytes()[..8].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    let generation = name[..8].parse().ok()?;
    (generation > 0).then_some(generation)
}

fn reject_symlink(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(Error::InvalidInput("generation path must not be a symlink"));
    }
    Ok(())
}

fn encode_manifest(generation: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(MANIFEST_V2_MAGIC);
    bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
    bytes[16..24].copy_from_slice(&generation.to_le_bytes());
    let checksum = crc32fast::hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

fn read_manifest(path: &Path) -> Result<u64> {
    reject_symlink(path)?;
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len == 16 {
        validate_header(&mut file, MANIFEST)?;
        return Ok(1);
    }
    if len != 32 {
        return Err(Error::Corruption { offset: 0 });
    }
    let mut bytes = [0u8; 32];
    file.read_exact(&mut bytes)?;
    if &bytes[..8] != MANIFEST_V2_MAGIC {
        return Err(Error::Corruption { offset: 0 });
    }
    let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    if version != 2 {
        return Err(Error::UnsupportedVersion(version));
    }
    if bytes[10..16] != [0; 6]
        || bytes[24..28] != [0; 4]
        || crc32fast::hash(&bytes[..28]) != u32::from_le_bytes(bytes[28..32].try_into().unwrap())
    {
        return Err(Error::Corruption { offset: 0 });
    }
    let generation = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    if generation == 0 || generation > MAX_GENERATION {
        return Err(Error::Corruption { offset: 0 });
    }
    Ok(generation)
}

fn read_point_bytes(
    file: &mut File,
    stats: &mut StorageStats,
    mut output: &mut [u8],
    offset: u64,
) -> Result<()> {
    while !output.is_empty() {
        match file.read(output) {
            Ok(0) => return Err(Error::IncompleteTail { offset }),
            Ok(count) => {
                stats.point_bytes = stats.point_bytes.saturating_add(count as u64);
                output = &mut output[count..];
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn backup_segment(path: &Path, segment: &mut File) -> Result<PathBuf> {
    // create_new prevents overwriting any earlier evidence, including after repeated crashes.
    for attempt in 0..10_000u32 {
        let backup = path.join(format!("recovery-{attempt:04}.segment.bak"));
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup);
        let mut destination = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        segment.seek(SeekFrom::Start(0))?;
        std::io::copy(segment, &mut destination)?;
        destination.sync_all()?;
        drop(destination);
        sync_dir(path)?;
        return Ok(backup);
    }
    Err(Error::InvalidInput("recovery backup limit reached"))
}

fn prefix_crc(segment: &mut File, end: u64) -> Result<u32> {
    segment.seek(SeekFrom::Start(0))?;
    let mut remaining = end;
    let mut hasher = crc32fast::Hasher::new();
    let mut buffer = [0u8; 8192];
    while remaining > 0 {
        let count = remaining.min(buffer.len() as u64) as usize;
        segment.read_exact(&mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hasher.finalize())
}

fn validate_checkpoint(
    path: &Path,
    segment: &mut File,
    entries: &[Arc<Entry>],
    generation: u64,
) -> Result<Option<bool>> {
    let mut file = match File::open(path.join("checkpoint.cbr")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let len = file.metadata()?.len();
    if len != 40 && len != 48 {
        return Ok(Some(false));
    }
    let mut bytes = [0u8; 48];
    file.read_exact(&mut bytes[..len as usize])?;
    let is_legacy = len == 40;
    let (magic, version, crc_offset) = if is_legacy {
        (b"CBRCHK01", 1u16, 36usize)
    } else {
        (b"CBRCHK02", 2u16, 44usize)
    };
    if &bytes[..8] != magic || bytes[8..10] != version.to_le_bytes() || bytes[10..16] != [0; 6] {
        return Ok(Some(false));
    }
    // v1 had no generation identity; never trust it after a generation rewrite.
    if (is_legacy && generation != 1)
        || (!is_legacy && u64::from_le_bytes(bytes[32..40].try_into().unwrap()) != generation)
    {
        return Ok(Some(false));
    }
    let expected = u32::from_le_bytes(bytes[crc_offset..crc_offset + 4].try_into().unwrap());
    if crc32fast::hash(&bytes[..crc_offset]) != expected {
        return Ok(Some(false));
    }
    let end = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let sequence = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
    if sequence > entries.len() as u64 {
        return Ok(Some(false));
    }
    let actual_end = entries[..sequence as usize]
        .iter()
        .try_fold(16u64, |offset, entry| {
            offset.checked_add((FRAME_OVERHEAD + entry.payload.len()) as u64)
        });
    if actual_end != Some(end) || end > segment.metadata()?.len() {
        return Ok(Some(false));
    }
    let payload_crc_offset = if is_legacy { 32 } else { 40 };
    let checksum = u32::from_le_bytes(
        bytes[payload_crc_offset..payload_crc_offset + 4]
            .try_into()
            .unwrap(),
    );
    Ok(Some(prefix_crc(segment, end)? == checksum))
}

fn replay(file: &mut File, stats: &mut StorageStats) -> Result<Replay> {
    #[cfg(feature = "memory-profile")]
    let _origin = canonical_memory::scope(canonical_memory::Component::StoreTemporary);
    let end = file.metadata()?.len();
    let mut offset = 16u64;
    let mut entries = Vec::new();
    let mut offsets = Vec::new();
    while offset < end {
        if end - offset < FRAME_HEADER as u64 {
            return Err(Error::IncompleteTail { offset });
        }
        let mut header = [0; FRAME_HEADER];
        file.read_exact(&mut header)?;
        // Verify header integrity before trusting its length or allocating a payload.
        // decode_frame with header alone either detects an invalid header or requests the body.
        match decode_frame(&header, offset) {
            Err(Error::IncompleteTail { .. }) => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(Error::Corruption { offset }),
        }
        // Even an incomplete body must belong to the next valid logical frame.
        // Otherwise corruption could be incorrectly treated as a repairable tail.
        let header_sequence = u64::from_le_bytes(header[12..20].try_into().unwrap());
        if header_sequence != entries.len() as u64 + 1 {
            return Err(Error::Corruption { offset });
        }
        let len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let total = len.checked_add(FRAME_OVERHEAD).ok_or(Error::TooLarge)?;
        if total > MAX_FRAME_BYTES {
            return Err(Error::TooLarge);
        }
        if end - offset < total as u64 {
            return Err(Error::IncompleteTail { offset });
        }
        let mut frame = Vec::with_capacity(total);
        frame.extend_from_slice(&header);
        frame.resize(total, 0);
        file.read_exact(&mut frame[FRAME_HEADER..])?;
        let (sequence, payload, _) = {
            #[cfg(feature = "memory-profile")]
            let _c = canonical_memory::scope(canonical_memory::Component::StorePayload);
            decode_frame(&frame, offset)?
        };
        if sequence != entries.len() as u64 + 1 {
            return Err(Error::Corruption { offset });
        }
        {
            #[cfg(feature = "memory-profile")]
            let _c = canonical_memory::scope(canonical_memory::Component::StoreContainers);
            entries.push(Arc::new(Entry { sequence, payload }));
            offsets.push(offset);
        }

        stats.replay_frames = stats.replay_frames.saturating_add(1);
        offset += total as u64;
    }
    Ok(Replay { entries, offsets })
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    // Windows std does not expose a portable directory flush. See documented durability boundary.
    Ok(())
}
