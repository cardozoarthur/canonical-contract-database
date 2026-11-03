//! CSR v1: immutable namespaces, concepts and schemas, with positional typed payloads.
//! Every registry fact is persisted in the same durable journal as its records.
use canonical_core::{Error, Result};
#[cfg(test)]
use canonical_index::PrimaryIndex;
use canonical_index::{
    CacheFormat, CacheValidation, IndexEntry, IndexKey, Location, save_entries_with_format,
    validate_cache_entries,
};
use canonical_storage::{AppendStage, Entry, RecoveryReport, StorageStats, Store};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

// Opt-in allocation origins only; default expansion preserves the original expression.
macro_rules! profile_memory {
    ($component:ident, $expression:expr) => {{
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::$component);
        $expression
    }};
}

mod canonical_locations;
mod history_read;
mod lazy_history;
mod metadata_capacity;
mod schema_bindings;
use lazy_history::{FrameSource, PayloadRange};
mod maintenance;
mod projection;
pub use projection::ProjectionDecoder;
#[cfg(feature = "memory-profile")]
mod memory;

pub const MAX_FIELDS: usize = 256;
pub const MAX_DEPTH: usize = 16;
pub const MAX_ARRAY: usize = 65_536;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const MAX_TRANSACTION_RECORDS: usize = 1024;
pub type TransactionId = [u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SchemaRef {
    pub namespace: u64,
    pub id: u64,
    pub version: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Reference {
    pub namespace: u64,
    pub id: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Level {
    Global,
    Plugin,
    Local,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namespace {
    pub id: u64,
    pub name: String,
    pub level: Level,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    pub namespace: u64,
    pub id: u64,
    pub version: u32,
    /// Opaque formal contract bytes, not a label-based equivalence heuristic.
    pub contract: Vec<u8>,
    pub label: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScalarType {
    Bool,
    UInt,
    Int,
    Utf8,
    Bytes,
    Ref,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Bool,
    UInt,
    Int,
    Decimal {
        scale: u8,
    },
    Float,
    Enum {
        variants: Vec<u32>,
    },
    Ref,
    Utf8,
    Bytes,
    Array {
        element: ScalarType,
        max_items: usize,
    },
    Struct(SchemaRef),
    Relation,
    FixedInt,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub label: String,
    pub kind: FieldType,
    pub optional: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub reference: SchemaRef,
    pub fields: Vec<Field>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ScalarValue {
    Bool(bool),
    UInt(u64),
    Int(i64),
    Utf8(String),
    Bytes(Vec<u8>),
    Ref(Reference),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    UInt(u64),
    Int(i64),
    Decimal {
        mantissa: i64,
        scale: u8,
    },
    Float(f64),
    Enum(u32),
    Ref(Reference),
    Utf8(String),
    Bytes(Vec<u8>),
    Array(Vec<ScalarValue>),
    /// An encoded payload of the schema referenced by FieldType::Struct.
    Struct(Vec<u8>),
    Relation {
        source: Reference,
        kind: u64,
        target: Reference,
    },
    FixedInt(#[serde(with = "fixed_int")] i64),
}

mod fixed_int {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        value.to_le_bytes().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        Ok(i64::from_le_bytes(<[u8; 8]>::deserialize(deserializer)?))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub namespace: u64,
    pub id: u64,
    pub schema: SchemaRef,
    pub revision: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NewRecord {
    pub namespace: u64,
    pub id: u64,
    pub schema: SchemaRef,
    pub values: Vec<Option<Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    Create(NewRecord),
    Update {
        record: NewRecord,
        expected_revision: u64,
    },
    Delete {
        namespace: u64,
        id: u64,
        expected_revision: u64,
    },
    Restore {
        record: NewRecord,
        expected_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub record: Record,
    pub deleted: bool,
    pub commit_sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationRule {
    pub schema: SchemaRef,
    pub kind: u64,
    pub source_schema: SchemaRef,
    pub target_schema: SchemaRef,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEdge {
    pub reference: Reference,
    pub revision: u64,
    pub schema: SchemaRef,
    pub kind: u64,
    pub source: Reference,
    pub target: Reference,
    pub commit_sequence: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum ChangeKind {
    Create,
    Update,
    Delete,
    Restore,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Change {
    kind: ChangeKind,
    expected_revision: u64,
    record: Record,
}
#[derive(Serialize, Deserialize)]
struct ChangeTransaction {
    id: TransactionId,
    changes: Vec<Change>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransactionReceipt {
    pub tx_id: TransactionId,
    pub commit_sequence: u64,
    pub records: usize,
}

type SnapshotRecords = BTreeMap<(u64, u64), Record>;
#[derive(Debug)]
struct SnapshotLease {
    records: Arc<SnapshotRecords>,
}
type SnapshotPins = Mutex<Vec<Weak<SnapshotLease>>>;
static SNAPSHOT_TRACKERS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<SnapshotPins>>>> = OnceLock::new();
fn snapshot_tracker(path: &Path) -> Result<Arc<SnapshotPins>> {
    let path = std::fs::canonicalize(path)?;
    let mut trackers = SNAPSHOT_TRACKERS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    trackers.retain(|_, v| v.strong_count() > 0);
    if let Some(tracker) = trackers.get(&path).and_then(Weak::upgrade) {
        return Ok(tracker);
    }
    let tracker = Arc::new(Mutex::new(Vec::new()));
    trackers.insert(path, Arc::downgrade(&tracker));
    Ok(tracker)
}
#[derive(Debug)]
#[cfg_attr(not(feature = "memory-profile"), derive(Clone))]
pub struct Snapshot {
    pub commit_sequence: u64,
    records: Arc<SnapshotRecords>,
    _reader: Arc<SnapshotLease>,
    _leases: Arc<SnapshotPins>,
}
#[cfg(feature = "memory-profile")]
impl Clone for Snapshot {
    fn clone(&self) -> Self {
        let cloned = Self {
            commit_sequence: self.commit_sequence,
            records: Arc::clone(&self.records),
            _reader: Arc::clone(&self._reader),
            _leases: Arc::clone(&self._leases),
        };
        assert!(
            Arc::ptr_eq(&cloned.records, &cloned._reader.records),
            "snapshot lease must guard its exact map"
        );
        memory::publish_snapshot_clone(&cloned);
        cloned
    }
}
impl Snapshot {
    pub fn record(&self, namespace: u64, id: u64) -> Option<&Record> {
        self.records.get(&(namespace, id))
    }
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }
}

#[derive(Serialize, Deserialize)]
struct Transaction {
    id: TransactionId,
    records: Vec<Record>,
}

// Nonrecursive wire types: nested structures reference an already registered schema and
// carry bounded payload bytes. This avoids unbounded recursive Serde schemas on disk.
#[derive(Serialize, Deserialize)]
struct Payload {
    bitmap: Vec<u8>,
    values: Vec<Value>,
}
#[derive(Serialize, Deserialize)]
enum Event {
    Namespace(Namespace),
    Concept(Concept),
    Schema(Schema),
    Record(Record),
    Transaction(Transaction),
    Changes(ChangeTransaction),
    Revision(Change),
    RelationRule(RelationRule),
    Compacted(PrunedEvent),
}
#[derive(Serialize, Deserialize)]
struct PrunedMember {
    change: Change,
    pruned: bool,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
enum PrunedKind {
    Record,
    Transaction,
    Changes,
    Revision,
}
#[derive(Serialize, Deserialize)]
struct PrunedEvent {
    version: u16,
    kind: PrunedKind,
    tx_id: Option<TransactionId>,
    fingerprint: Option<[u8; 32]>,
    members: Vec<PrunedMember>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Retention {
    #[default]
    KeepAll,
    KeepLast(usize),
}
#[derive(Debug)]
pub struct CompactionReport {
    pub generation: canonical_storage::GenerationReport,
    pub retained_revisions: usize,
    pub pruned_revisions: usize,
    pub already_pruned_revisions: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionStatus {
    Available,
    Pruned,
    Missing,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u16,
    event: Event,
}

/// Private timeline retains pruned slots without owning record identities or payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HistoryMeta {
    schema_ordinal: usize,
    payload_start: u32,
    payload_len: u32,
    commit_sequence: u64,
    member: u16,
    deleted: bool,
}

struct State {
    namespaces: BTreeMap<u64, Namespace>,
    concepts: BTreeMap<(u64, u64, u32), Concept>,
    schemas: BTreeMap<SchemaRef, Schema>,
    schema_table: Vec<SchemaRef>,
    schema_ordinals: BTreeMap<SchemaRef, usize>,
    records: Arc<SnapshotRecords>,
    transactions: BTreeMap<TransactionId, (TransactionReceipt, [u8; 32])>,
    change_transactions: BTreeMap<TransactionId, (TransactionReceipt, [u8; 32])>,
    history: BTreeMap<(u64, u64), OnceLock<Box<[Revision]>>>,
    frames: Vec<FrameSource>,
    #[cfg(feature = "memory-profile")]
    history_cache_stats: Mutex<memory::HistoryCacheStats>,
    history_meta: BTreeMap<(u64, u64), Vec<HistoryMeta>>,
    pruned: BTreeSet<(u64, u64, u64)>,
    relation_rules: BTreeMap<SchemaRef, RelationRule>,
    native_edges: BTreeMap<Reference, NativeEdge>,
    by_source: BTreeMap<Reference, std::collections::BTreeSet<Reference>>,
    by_target: BTreeMap<Reference, std::collections::BTreeSet<Reference>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            namespaces: Default::default(),
            concepts: Default::default(),
            schemas: Default::default(),
            schema_table: Default::default(),
            schema_ordinals: Default::default(),
            records: profile_memory!(RegistryCurrent, Arc::new(BTreeMap::new())),
            transactions: Default::default(),
            change_transactions: Default::default(),
            history: Default::default(),
            frames: Default::default(),
            #[cfg(feature = "memory-profile")]
            history_cache_stats: Default::default(),
            history_meta: Default::default(),
            pruned: Default::default(),
            relation_rules: Default::default(),
            native_edges: Default::default(),
            by_source: Default::default(),
            by_target: Default::default(),
        }
    }
}

/// Persistence of a derived cache; authoritative journal replay is never optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IndexPolicy {
    #[default]
    Full,
    Grouped,
    Delta,
    ReplayOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexRebuildReason {
    Missing,
    InvalidOrStale,
    ContentMismatch,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexStatus {
    /// Cache matched every authoritative replay entry; no cached BTreeMap is retained.
    pub loaded: bool,
    pub rebuilt: bool,
    pub reason: Option<IndexRebuildReason>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LookupMetrics {
    pub index_probes: u64,
    pub point_reads: u64,
    pub point_bytes: u64,
    pub frames_scanned: u64,
    /// Members materialized by the decoder, not all slots visited. Shared batch work
    /// is attributed to the first request for that frame, and zero to its later requests.
    pub decoded_members: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupResult {
    pub revision: Option<Revision>,
    pub metrics: LookupMetrics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LookupRequest {
    pub namespace: u64,
    pub id: u64,
    pub revision: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryReadMode {
    ReferenceFull,
    Selective,
    Batch,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryReadMetrics {
    pub physical_frame_reads: u64,
    pub physical_bytes_read: u64,
    /// Unique header CRC domain (20 bytes/frame). The header is hashed twice by
    /// the physical header/full-frame validation path; this is not a hash-work counter.
    pub header_crc_bytes_verified: u64,
    pub payload_crc_bytes_verified: u64,
    pub prefix_payload_bytes_compared: u64,
    pub members_visited: u64,
    pub members_decoded: u64,
    pub output_records: u64,
    pub output_payload_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupBatchResult {
    pub results: Vec<LookupResult>,
    pub metrics: HistoryReadMetrics,
}

pub struct Registry {
    journal: Store,
    state: State,
    sequence: u64,
    index_path: PathBuf,
    index_status: IndexStatus,
    index_policy: IndexPolicy,
    needs_reopen: bool,
    snapshots: Arc<SnapshotPins>,
}

impl Registry {
    /// Explicit policy controls future flushes, not whether authoritative replay is performed.
    pub fn open_with_index_policy(path: impl AsRef<Path>, policy: IndexPolicy) -> Result<Self> {
        let path = path.as_ref();
        let journal = Store::open(path)?;
        Self::from_journal(journal, path, policy)
    }
    /// Recover an incomplete authoritative tail before considering any derived cache.
    pub fn recover_with_index_policy(
        path: impl AsRef<Path>,
        policy: IndexPolicy,
    ) -> Result<(Self, RecoveryReport)> {
        let path = path.as_ref();
        let (journal, report) = Store::recover(path)?;
        Ok((Self::from_journal(journal, path, policy)?, report))
    }
    pub fn index_policy(&self) -> IndexPolicy {
        self.index_policy
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_index_policy(path, IndexPolicy::Full)
    }
    /// Explicit recovery preserves a backup before removing incomplete tail bytes.
    pub fn recover(path: impl AsRef<Path>) -> Result<(Self, RecoveryReport)> {
        Self::recover_with_index_policy(path, IndexPolicy::Full)
    }
    fn from_journal(journal: Store, path: &Path, policy: IndexPolicy) -> Result<Self> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        #[cfg(feature = "memory-profile")]
        memory::begin_open();
        let mut state = State::default();
        let mut sequence = 0;
        for entry in journal.snapshot() {
            let envelope: Envelope = canonical_codec::decode(&entry.payload)?;
            if envelope.version != 1 {
                return Err(Error::UnsupportedVersion(envelope.version));
            }
            state.validate(&envelope.event)?;
            let offset = journal
                .frame_offset(entry.sequence)
                .ok_or(Error::Corruption { offset: 0 })?;
            state.apply(
                envelope.event,
                entry.sequence,
                Some(offset),
                Some(Arc::clone(&entry)),
                &entry.payload,
            )?;
            sequence = entry.sequence;
        }
        state.validate_heads()?;
        state.trim_replayed_metadata();
        let index_path = path.join("indexes/primary.cbi");
        let validation = if policy == IndexPolicy::ReplayOnly {
            None
        } else {
            Some(profile_memory!(
                PrimaryCache,
                validate_cache_entries(
                    &index_path,
                    sequence,
                    state.canonical_entry_count()?,
                    state.canonical_entries()
                )
            ))
        };
        let index_status = match validation {
            None => IndexStatus {
                loaded: false,
                rebuilt: true,
                reason: None,
            },
            Some(Ok(CacheValidation::Match)) => IndexStatus {
                loaded: true,
                rebuilt: false,
                reason: None,
            },
            Some(Ok(CacheValidation::ContentMismatch)) => IndexStatus {
                loaded: false,
                rebuilt: true,
                reason: Some(IndexRebuildReason::ContentMismatch),
            },
            Some(Err(Error::Io(error))) => {
                let reason = if error.kind() == std::io::ErrorKind::NotFound {
                    IndexRebuildReason::Missing
                } else {
                    IndexRebuildReason::Unavailable
                };
                IndexStatus {
                    loaded: false,
                    rebuilt: true,
                    reason: Some(reason),
                }
            }
            Some(Err(_)) => IndexStatus {
                loaded: false,
                rebuilt: true,
                reason: Some(IndexRebuildReason::InvalidOrStale),
            },
        };
        #[cfg(feature = "memory-profile")]
        memory::publish_primary_validation(
            state.canonical_entry_count()?,
            policy != IndexPolicy::ReplayOnly,
            index_status.loaded,
        );
        let registry = Self {
            journal,
            state,
            sequence,
            index_path,
            index_status,
            index_policy: policy,
            needs_reopen: false,
            snapshots: profile_memory!(RegistrySnapshots, snapshot_tracker(path)?),
        };
        #[cfg(feature = "memory-profile")]
        memory::publish_registry(&registry);
        Ok(registry)
    }
    fn ensure_healthy(&self) -> Result<()> {
        if self.needs_reopen {
            return Err(Error::Poisoned);
        }
        self.journal.ensure_healthy()
    }
    fn publish(&mut self, event: Event, sequence: u64) -> Result<()> {
        #[cfg(feature = "memory-profile")]
        let schema_changed = matches!(&event, Event::Schema(_));
        let result = (|| {
            let offset = self
                .journal
                .frame_offset(sequence)
                .ok_or(Error::Corruption { offset: 0 })?;
            let entry = self
                .journal
                .prefix_entry(sequence)
                .ok_or(Error::Corruption { offset })?;
            self.state.apply(
                event,
                sequence,
                Some(offset),
                Some(Arc::clone(&entry)),
                &entry.payload,
            )
        })();
        if let Err(error) = result {
            self.needs_reopen = true;
            return Err(error);
        }
        self.sequence = sequence;
        #[cfg(feature = "memory-profile")]
        memory::publish_history_cache(&self.state);
        #[cfg(feature = "memory-profile")]
        if schema_changed {
            memory::publish_schema_table(&self.state);
        }
        Ok(())
    }
    fn persist(&mut self, event: Event) -> Result<()> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.state.validate(&event)?;
        let envelope = Envelope { version: 1, event };
        let payload = canonical_codec::encode(&envelope)?;
        let receipt = self.journal.append(&payload)?;
        self.publish(envelope.event, receipt.sequence)?;
        Ok(())
    }
    pub fn register_namespace(&mut self, namespace: Namespace) -> Result<()> {
        self.ensure_healthy()?;
        if self.state.namespaces.get(&namespace.id) == Some(&namespace) {
            return Ok(());
        }
        self.persist(Event::Namespace(namespace))
    }
    pub fn register_concept(&mut self, concept: Concept) -> Result<()> {
        self.ensure_healthy()?;
        let key = (concept.namespace, concept.id, concept.version);
        if self.state.concepts.get(&key) == Some(&concept) {
            return Ok(());
        }
        self.persist(Event::Concept(concept))
    }
    pub fn register_schema(&mut self, schema: Schema) -> Result<()> {
        self.ensure_healthy()?;
        if self.state.schemas.get(&schema.reference) == Some(&schema) {
            return Ok(());
        }
        self.persist(Event::Schema(schema))
    }
    pub fn register_relation_rule(&mut self, rule: RelationRule) -> Result<()> {
        self.ensure_healthy()?;
        if self.state.relation_rules.get(&rule.schema) == Some(&rule) {
            return Ok(());
        }
        self.persist(Event::RelationRule(rule))
    }
    pub fn relation_rule(&self, schema: SchemaRef) -> Option<&RelationRule> {
        self.state.relation_rules.get(&schema)
    }
    pub fn relation_rules(&self) -> impl Iterator<Item = &RelationRule> {
        self.state.relation_rules.values()
    }
    pub fn native_edge(&self, reference: Reference) -> Option<&NativeEdge> {
        self.state.native_edges.get(&reference)
    }
    pub fn namespace(&self, id: u64) -> Option<&Namespace> {
        self.state.namespaces.get(&id)
    }
    pub fn concept(&self, namespace: u64, id: u64, version: u32) -> Option<&Concept> {
        self.state.concepts.get(&(namespace, id, version))
    }
    pub fn schema(&self, reference: SchemaRef) -> Result<&Schema> {
        self.state
            .schemas
            .get(&reference)
            .ok_or(Error::UnknownSchema)
    }
    pub fn schemas(&self) -> impl Iterator<Item = &Schema> {
        self.state.schemas.values()
    }
    pub fn encode_values(&self, schema: SchemaRef, values: &[Option<Value>]) -> Result<Vec<u8>> {
        self.state.encode_values(schema, values, 0)
    }
    pub fn decode_values(&self, schema: SchemaRef, payload: &[u8]) -> Result<Vec<Option<Value>>> {
        self.state.decode_values(schema, payload, 0)
    }

    /// Freeze only the schema and namespace catalogs for independent payload projections.
    pub fn projection_decoder(&self) -> ProjectionDecoder {
        ProjectionDecoder::new(&self.state)
    }
    pub fn create(
        &mut self,
        namespace: u64,
        id: u64,
        schema: SchemaRef,
        values: &[Option<Value>],
    ) -> Result<Record> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.ensure_healthy()?;
        let payload = self.encode_values(schema, values)?;
        let record = Record {
            namespace,
            id,
            schema,
            revision: 1,
            payload,
        };
        self.persist(Event::Record(record.clone()))?;
        Ok(record)
    }
    pub fn record(&self, namespace: u64, id: u64) -> Option<&Record> {
        self.state.records.get(&(namespace, id))
    }
    pub fn decode_record(&self, namespace: u64, id: u64) -> Result<Vec<Option<Value>>> {
        let record = self.record(namespace, id).ok_or(Error::NotFound)?;
        self.decode_values(record.schema, &record.payload)
    }

    /// A transaction is one complete, bounded checksummed frame. No observer can see a subset.
    pub fn create_batch(
        &mut self,
        tx_id: TransactionId,
        records: &[NewRecord],
    ) -> Result<TransactionReceipt> {
        self.create_batch_with_hook(tx_id, records, |_| Ok(()))
    }

    /// Diagnostic seam for deterministic failure injection. Hooks run inside the write protocol.
    #[doc(hidden)]
    pub fn create_batch_with_hook<F>(
        &mut self,
        tx_id: TransactionId,
        records: &[NewRecord],
        hook: F,
    ) -> Result<TransactionReceipt>
    where
        F: FnMut(AppendStage) -> Result<()>,
    {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.ensure_healthy()?;
        if tx_id == [0; 16] || records.is_empty() || records.len() > MAX_TRANSACTION_RECORDS {
            return Err(invalid("transaction identity or cardinality invalid"));
        }
        // Bound the aggregate while preparing, before retaining a potentially huge batch.
        // 32 bytes reserves transaction envelope/id/count overhead (actual framing is checked too).
        let mut prepared = Vec::with_capacity(records.len());
        let mut total = 32usize;
        for r in records {
            let record = Record {
                namespace: r.namespace,
                id: r.id,
                schema: r.schema,
                revision: 1,
                payload: self.encode_values(r.schema, &r.values)?,
            };
            total = total
                .checked_add(canonical_codec::encode(&record)?.len())
                .ok_or(Error::TooLarge)?;
            if total > canonical_core::MAX_FRAME_BYTES - canonical_codec::FRAME_OVERHEAD {
                return Err(Error::TooLarge);
            }
            prepared.push(record);
        }
        let records = prepared;
        if let Some((receipt, previous)) = self.state.transactions.get(&tx_id) {
            return if *previous == fingerprint(&records)? {
                Ok(*receipt)
            } else {
                Err(Error::Conflict)
            };
        }
        if self.state.change_transactions.contains_key(&tx_id) {
            return Err(Error::Conflict);
        }
        let event = Event::Transaction(Transaction { id: tx_id, records });
        self.state.validate(&event)?;
        let envelope = Envelope { version: 1, event };
        let bytes = canonical_codec::encode(&envelope)?;
        let receipt = self.journal.append_with_hook(&bytes, hook)?;
        self.publish(envelope.event, receipt.sequence)?;
        Ok(self.state.transactions[&tx_id].0)
    }
    pub fn transaction_receipt(&self, tx_id: TransactionId) -> Option<TransactionReceipt> {
        self.state
            .transactions
            .get(&tx_id)
            .map(|v| v.0)
            .or_else(|| self.state.change_transactions.get(&tx_id).map(|v| v.0))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.pin_snapshot(self.sequence, Arc::clone(&self.state.records))
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        self.journal.checkpoint()?;
        self.flush_index()
    }

    pub fn storage_stats(&self) -> StorageStats {
        self.journal.io_stats()
    }
    pub fn index_status(&self) -> IndexStatus {
        self.index_status
    }
    /// Persist derived state explicitly. Failure does not invalidate a confirmed data commit.
    pub fn flush_index(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        let format = match self.index_policy {
            IndexPolicy::Full => CacheFormat::Legacy,
            IndexPolicy::Grouped => CacheFormat::Grouped,
            IndexPolicy::Delta => CacheFormat::Delta,
            IndexPolicy::ReplayOnly => return Ok(()),
        };
        std::fs::create_dir_all(self.index_path.parent().expect("index parent"))?;
        save_entries_with_format(
            &self.index_path,
            self.sequence,
            self.state.canonical_entry_count()?,
            format,
            self.state.canonical_entries(),
        )
    }
    /// Bounded historical reads: at most 1024 requests and 16 MiB of output payloads,
    /// counting duplicates. Results preserve request order; failures return no partial page.
    /// Batch shares physical proof only within this call; every later call rereads disk.
    pub fn lookup_many(
        &mut self,
        requests: &[LookupRequest],
        mode: HistoryReadMode,
    ) -> Result<LookupBatchResult> {
        self.read_history_many(requests, mode)
    }

    /// Resolve one revision physically, materializing only its selected member.
    pub fn lookup(
        &mut self,
        namespace: u64,
        id: u64,
        revision: Option<u64>,
    ) -> Result<LookupResult> {
        let mut batch = self.lookup_many(
            &[LookupRequest {
                namespace,
                id,
                revision,
            }],
            HistoryReadMode::Selective,
        )?;
        Ok(batch.results.remove(0))
    }

    pub fn history(&self, namespace: u64, id: u64) -> &[Revision] {
        self.state.public_history(namespace, id)
    }
    pub fn revision(&self, namespace: u64, id: u64, revision: u64) -> Option<&Revision> {
        let history = self.history(namespace, id);
        history
            .binary_search_by_key(&revision, |v| v.record.revision)
            .ok()
            .map(|i| &history[i])
    }
    pub fn revision_status(&self, namespace: u64, id: u64, revision: u64) -> RevisionStatus {
        if self.state.pruned.contains(&(namespace, id, revision)) {
            RevisionStatus::Pruned
        } else if revision
            .checked_sub(1)
            .and_then(|slot| usize::try_from(slot).ok())
            .and_then(|slot| {
                self.state
                    .history_meta
                    .get(&(namespace, id))
                    .and_then(|h| h.get(slot))
            })
            .is_some()
        {
            RevisionStatus::Available
        } else {
            RevisionStatus::Missing
        }
    }
    fn pin_snapshot(&self, commit_sequence: u64, records: Arc<SnapshotRecords>) -> Snapshot {
        #[cfg(feature = "memory-profile")]
        let _memory_origin =
            canonical_memory::scope(canonical_memory::Component::RegistrySnapshots);
        let reader = Arc::new(SnapshotLease {
            records: Arc::clone(&records),
        });
        let mut pins = self.snapshots.lock().unwrap_or_else(|e| e.into_inner());
        pins.retain(|v| v.strong_count() > 0);
        pins.push(Arc::downgrade(&reader));
        let snapshot = Snapshot {
            commit_sequence,
            records,
            _reader: reader,
            _leases: self.snapshots.clone(),
        };
        assert!(
            Arc::ptr_eq(&snapshot.records, &snapshot._reader.records),
            "snapshot lease must guard its exact map"
        );
        drop(pins);
        #[cfg(feature = "memory-profile")]
        {
            memory::publish_snapshot(&snapshot);
            memory::publish_registry(self);
        }
        snapshot
    }
    pub fn snapshot_at(&self, commit_sequence: u64) -> Result<Snapshot> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin =
            canonical_memory::scope(canonical_memory::Component::RegistrySnapshots);
        if commit_sequence > self.sequence {
            return Err(invalid("snapshot exceeds committed prefix"));
        }
        let mut records = BTreeMap::new();
        for (key, history) in &self.state.history_meta {
            if let Some((slot, meta)) = history
                .iter()
                .enumerate()
                .rev()
                .find(|(_, v)| v.commit_sequence <= commit_sequence)
            {
                if meta.deleted {
                    continue;
                }
                let version = self
                    .state
                    .expected_revision(
                        key.0,
                        key.1,
                        u64::try_from(slot)
                            .map_err(|_| Error::TooLarge)?
                            .checked_add(1)
                            .ok_or(Error::TooLarge)?,
                    )
                    .ok_or(Error::HistoryPruned)?;
                records.insert(*key, version.record.owned());
            }
        }
        Ok(self.pin_snapshot(
            commit_sequence,
            profile_memory!(RegistrySnapshots, Arc::new(records)),
        ))
    }
    fn prepare_change(&self, mutation: &Mutation) -> Result<Change> {
        let (kind, input, expected) = match mutation {
            Mutation::Create(record) => (ChangeKind::Create, record, 0),
            Mutation::Update {
                record,
                expected_revision,
            } => (ChangeKind::Update, record, *expected_revision),
            Mutation::Restore {
                record,
                expected_revision,
            } => (ChangeKind::Restore, record, *expected_revision),
            Mutation::Delete {
                namespace,
                id,
                expected_revision,
            } => {
                if !self.state.history_meta.contains_key(&(*namespace, *id)) {
                    return Err(Error::NotFound);
                }
                let prior = self
                    .state
                    .history_meta
                    .get(&(*namespace, *id))
                    .and_then(|h| {
                        expected_revision
                            .checked_sub(1)
                            .and_then(|slot| usize::try_from(slot).ok())
                            .and_then(|slot| h.get(slot))
                    })
                    .ok_or(Error::Conflict)?;
                return Ok(Change {
                    kind: ChangeKind::Delete,
                    expected_revision: *expected_revision,
                    record: Record {
                        namespace: *namespace,
                        id: *id,
                        schema: self
                            .state
                            .metadata_schema(prior)
                            .ok_or(Error::UnknownSchema)?,
                        revision: expected_revision
                            .checked_add(1)
                            .ok_or(invalid("revision exhausted"))?,
                        payload: Vec::new(),
                    },
                });
            }
        };
        Ok(Change {
            kind,
            expected_revision: expected,
            record: Record {
                namespace: input.namespace,
                id: input.id,
                schema: input.schema,
                revision: expected
                    .checked_add(1)
                    .ok_or(invalid("revision exhausted"))?,
                payload: self.encode_values(input.schema, &input.values)?,
            },
        })
    }
    fn mutate_single(&mut self, mutation: Mutation) -> Result<Revision> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.ensure_healthy()?;
        let change = self.prepare_change(&mutation)?;
        let record = change.record.clone();
        let deleted = change.kind == ChangeKind::Delete;
        self.persist(Event::Revision(change))?;
        Ok(Revision {
            record,
            deleted,
            commit_sequence: self.sequence,
        })
    }
    pub fn update(&mut self, record: NewRecord, expected_revision: u64) -> Result<Record> {
        Ok(self
            .mutate_single(Mutation::Update {
                record,
                expected_revision,
            })?
            .record)
    }
    pub fn delete(&mut self, namespace: u64, id: u64, expected_revision: u64) -> Result<Revision> {
        self.mutate_single(Mutation::Delete {
            namespace,
            id,
            expected_revision,
        })
    }
    pub fn restore(&mut self, record: NewRecord, expected_revision: u64) -> Result<Record> {
        Ok(self
            .mutate_single(Mutation::Restore {
                record,
                expected_revision,
            })?
            .record)
    }
    pub fn commit(
        &mut self,
        tx_id: TransactionId,
        mutations: &[Mutation],
    ) -> Result<TransactionReceipt> {
        self.commit_with_hook(tx_id, mutations, |_| Ok(()))
    }
    #[doc(hidden)]
    pub fn commit_with_hook<F>(
        &mut self,
        tx_id: TransactionId,
        mutations: &[Mutation],
        hook: F,
    ) -> Result<TransactionReceipt>
    where
        F: FnMut(AppendStage) -> Result<()>,
    {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryDecode);
        self.ensure_healthy()?;
        if tx_id == [0; 16] || mutations.is_empty() || mutations.len() > MAX_TRANSACTION_RECORDS {
            return Err(invalid("transaction bounds"));
        }
        if self.state.transactions.contains_key(&tx_id) {
            return Err(Error::Conflict);
        }
        let mut changes = Vec::with_capacity(mutations.len());
        let mut total = 32usize;
        for mutation in mutations {
            let change = self.prepare_change(mutation)?;
            total = total
                .checked_add(canonical_codec::encode(&change)?.len())
                .ok_or(Error::TooLarge)?;
            if total > canonical_core::MAX_FRAME_BYTES - canonical_codec::FRAME_OVERHEAD {
                return Err(Error::TooLarge);
            }
            changes.push(change);
        }
        if let Some((receipt, previous)) = self.state.change_transactions.get(&tx_id) {
            return if *previous == fingerprint(&changes)? {
                Ok(*receipt)
            } else {
                Err(Error::Conflict)
            };
        }
        let event = Event::Changes(ChangeTransaction { id: tx_id, changes });
        self.state.validate(&event)?;
        let envelope = Envelope { version: 1, event };
        let payload = canonical_codec::encode(&envelope)?;
        let receipt = self.journal.append_with_hook(&payload, hook)?;
        self.publish(envelope.event, receipt.sequence)?;
        Ok(self.state.change_transactions[&tx_id].0)
    }
}

fn event_members(event: &Event) -> Vec<(&Record, bool)> {
    match event {
        Event::Record(record) => vec![(record, false)],
        Event::Transaction(tx) => tx.records.iter().map(|r| (r, false)).collect(),
        Event::Changes(tx) => tx
            .changes
            .iter()
            .map(|c| (&c.record, c.kind == ChangeKind::Delete))
            .collect(),
        Event::Revision(change) => vec![(&change.record, change.kind == ChangeKind::Delete)],
        Event::Compacted(tx) => tx
            .members
            .iter()
            .map(|m| (&m.change.record, m.change.kind == ChangeKind::Delete))
            .collect(),
        _ => Vec::new(),
    }
}
#[cfg(test)]
fn index_event(index: &mut PrimaryIndex, event: &Event, sequence: u64, offset: u64) -> Result<()> {
    #[cfg(feature = "memory-profile")]
    let _memory_origin = canonical_memory::scope(canonical_memory::Component::PrimaryReplay);
    for (member, (record, deleted)) in event_members(event).into_iter().enumerate() {
        if let Event::Compacted(tx) = event
            && tx.members[member].pruned
        {
            continue;
        }
        index.insert_retained(IndexEntry {
            key: IndexKey {
                namespace: record.namespace,
                id: record.id,
                revision: record.revision,
            },
            location: Location {
                commit_sequence: sequence,
                frame_offset: offset,
                member: u32::try_from(member).map_err(|_| Error::TooLarge)?,
                deleted,
            },
        })?;
    }
    Ok(())
}

fn invalid(message: &'static str) -> Error {
    Error::InvalidInput(message)
}
fn label(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 1024 {
        Err(invalid("label length out of bounds"))
    } else {
        Ok(())
    }
}
fn reference(r: Reference) -> Result<()> {
    if r.id == 0 {
        Err(invalid("zero reference id"))
    } else {
        Ok(())
    }
}
impl State {
    fn require_namespace(&self, ns: u64) -> Result<()> {
        if self.namespaces.contains_key(&ns) {
            Ok(())
        } else {
            Err(invalid("unregistered namespace"))
        }
    }
    fn validate(&self, event: &Event) -> Result<()> {
        match event {
            Event::Namespace(ns) => {
                label(&ns.name)?;
                if self.namespaces.contains_key(&ns.id) {
                    return Err(Error::Conflict);
                }
                if self.namespaces.values().any(|v| v.name == ns.name) {
                    return Err(Error::Conflict);
                }
            }
            Event::Concept(concept) => {
                self.require_namespace(concept.namespace)?;
                if concept.id == 0
                    || concept.version == 0
                    || concept.contract.is_empty()
                    || concept.contract.len() > 65_536
                {
                    return Err(invalid("concept bounds"));
                }
                if let Some(s) = &concept.label {
                    label(s)?;
                }
                if self
                    .concepts
                    .contains_key(&(concept.namespace, concept.id, concept.version))
                {
                    return Err(Error::Conflict);
                }
            }
            Event::Schema(schema) => {
                self.require_namespace(schema.reference.namespace)?;
                if self.schemas.contains_key(&schema.reference) {
                    return Err(Error::Conflict);
                }
                if schema.reference.id == 0
                    || schema.reference.version == 0
                    || schema.fields.len() > MAX_FIELDS
                {
                    return Err(invalid("schema bounds"));
                }
                let mut names = std::collections::BTreeSet::new();
                for field in &schema.fields {
                    label(&field.label)?;
                    if !names.insert(&field.label) {
                        return Err(invalid("duplicate field label"));
                    }
                    match &field.kind {
                        FieldType::Decimal { scale } if *scale > 18 => {
                            return Err(invalid("decimal scale exceeds 18"));
                        }
                        FieldType::Enum { variants } => {
                            if variants.is_empty()
                                || variants.len() > MAX_ARRAY
                                || variants
                                    .iter()
                                    .collect::<std::collections::BTreeSet<_>>()
                                    .len()
                                    != variants.len()
                            {
                                return Err(invalid("enum variants invalid"));
                            }
                        }
                        FieldType::Array { max_items, .. } if *max_items > MAX_ARRAY => {
                            return Err(invalid("array schema limit"));
                        }
                        FieldType::Struct(child) => {
                            self.schema_depth(*child, 1)?;
                        }
                        _ => (),
                    }
                }
            }
            Event::Record(record) => {
                self.validate_new_record(record)?;
            }
            Event::Transaction(tx) => {
                if tx.id == [0; 16]
                    || tx.records.is_empty()
                    || tx.records.len() > MAX_TRANSACTION_RECORDS
                {
                    return Err(invalid("transaction bounds"));
                }
                if self.transactions.contains_key(&tx.id)
                    || self.change_transactions.contains_key(&tx.id)
                {
                    return Err(Error::Conflict);
                }
                let mut keys = std::collections::BTreeSet::new();
                for record in &tx.records {
                    if !keys.insert((record.namespace, record.id)) {
                        return Err(Error::Conflict);
                    }
                    self.validate_new_record(record)?;
                }
            }
            Event::Changes(tx) => {
                if tx.id == [0; 16]
                    || tx.changes.is_empty()
                    || tx.changes.len() > MAX_TRANSACTION_RECORDS
                {
                    return Err(invalid("transaction bounds"));
                }
                if self.transactions.contains_key(&tx.id)
                    || self.change_transactions.contains_key(&tx.id)
                {
                    return Err(Error::Conflict);
                }
                let mut keys = std::collections::BTreeSet::new();
                for change in &tx.changes {
                    if !keys.insert((change.record.namespace, change.record.id)) {
                        return Err(Error::Conflict);
                    }
                    self.validate_change(change)?;
                }
            }
            Event::Revision(change) => self.validate_change(change)?,
            Event::Compacted(tx) => self.validate_pruned(tx)?,
            Event::RelationRule(rule) => {
                if self.relation_rules.contains_key(&rule.schema) {
                    return Err(Error::Conflict);
                }
                if rule.kind == 0 {
                    return Err(invalid("relation kind must be nonzero"));
                }
                let schema = self.schemas.get(&rule.schema).ok_or(Error::UnknownSchema)?;
                if schema.fields.len() != 1
                    || schema.fields[0].optional
                    || schema.fields[0].kind != FieldType::Relation
                {
                    return Err(invalid(
                        "native relation schema must have one required Relation field",
                    ));
                }
                for sr in [rule.source_schema, rule.target_schema] {
                    if !self.schemas.contains_key(&sr) {
                        return Err(Error::UnknownSchema);
                    }
                }
                if self.relation_rules.values().any(|old| {
                    old.schema.namespace == rule.schema.namespace && old.kind == rule.kind
                }) {
                    return Err(Error::Conflict);
                }
                if self
                    .history_meta
                    .values()
                    .flatten()
                    .any(|v| self.metadata_schema(v) == Some(rule.schema))
                {
                    return Err(Error::Conflict);
                }
            }
        }
        self.validate_relation_changes(event)?;
        Ok(())
    }
    fn native_from_record(&self, record: &Record, sequence: u64) -> Result<Option<NativeEdge>> {
        let Some(rule) = self.relation_rules.get(&record.schema) else {
            return Ok(None);
        };
        let values = self.decode_values(record.schema, &record.payload, 0)?;
        let [
            Some(Value::Relation {
                source,
                kind,
                target,
            }),
        ] = values.as_slice()
        else {
            return Err(invalid("invalid native edge payload"));
        };
        if *kind != rule.kind {
            return Err(invalid("relation kind does not match immutable rule"));
        }
        Ok(Some(NativeEdge {
            reference: Reference {
                namespace: record.namespace,
                id: record.id,
            },
            revision: record.revision,
            schema: record.schema,
            kind: *kind,
            source: *source,
            target: *target,
            commit_sequence: sequence,
        }))
    }
    fn validate_relation_changes(&self, event: &Event) -> Result<()> {
        if self.relation_rules.is_empty() {
            return Ok(());
        }
        let members = event_members(event);
        if members.is_empty() {
            return Ok(());
        }
        let projected: BTreeMap<Reference, Option<&Record>> = members
            .iter()
            .map(|(record, deleted)| {
                (
                    Reference {
                        namespace: record.namespace,
                        id: record.id,
                    },
                    if *deleted { None } else { Some(*record) },
                )
            })
            .collect();
        let mut impacted = std::collections::BTreeSet::new();
        for (key, record) in &projected {
            if let Some(edges) = self.by_source.get(key) {
                impacted.extend(edges.iter().copied());
            }
            if let Some(edges) = self.by_target.get(key) {
                impacted.extend(edges.iter().copied());
            }
            if self.native_edges.contains_key(key)
                || record.is_some_and(|r| self.relation_rules.contains_key(&r.schema))
            {
                impacted.insert(*key);
            }
        }
        let current = |key: Reference| {
            projected
                .get(&key)
                .copied()
                .unwrap_or_else(|| self.records.get(&(key.namespace, key.id)))
        };
        for reference in impacted {
            let Some(record) = current(reference) else {
                continue;
            };
            let Some(edge) = self.native_from_record(record, 0)? else {
                continue;
            };
            let rule = &self.relation_rules[&record.schema];
            for (endpoint, expected) in [
                (edge.source, rule.source_schema),
                (edge.target, rule.target_schema),
            ] {
                match current(endpoint) {
                    Some(node) if node.schema == expected => (),
                    Some(_) if self.native_edges.contains_key(&reference) => {
                        return Err(Error::Conflict);
                    }
                    Some(_) => return Err(invalid("native relation endpoint schema mismatch")),
                    None if self.native_edges.contains_key(&reference) => {
                        return Err(Error::Conflict);
                    }
                    None => return Err(Error::NotFound),
                }
            }
        }
        Ok(())
    }
    fn validate_new_record(&self, record: &Record) -> Result<()> {
        self.validate_new_inner(record, false)
    }
    fn validate_new_inner(&self, record: &Record, pruned: bool) -> Result<()> {
        self.require_namespace(record.namespace)?;
        if record.id == 0 || record.revision != 1 {
            return Err(invalid("record identity/revision invalid"));
        }
        if self
            .history_meta
            .contains_key(&(record.namespace, record.id))
        {
            return Err(Error::Conflict);
        }
        if !pruned {
            self.decode_values(record.schema, &record.payload, 0)?;
        }
        Ok(())
    }
    fn validate_change(&self, change: &Change) -> Result<()> {
        self.validate_change_inner(change, false)
    }
    fn validate_change_inner(&self, change: &Change, pruned: bool) -> Result<()> {
        let record = &change.record;
        self.require_namespace(record.namespace)?;
        if record.id == 0
            || record.revision
                != change
                    .expected_revision
                    .checked_add(1)
                    .ok_or(invalid("revision exhausted"))?
        {
            return Err(invalid("invalid revision transition"));
        }
        if change.kind == ChangeKind::Create {
            if change.expected_revision != 0 {
                return Err(invalid("create revision must be zero"));
            }
            return self.validate_new_inner(record, pruned);
        }
        let timeline = self
            .history_meta
            .get(&(record.namespace, record.id))
            .ok_or(Error::NotFound)?;
        let previous = timeline.last().ok_or(Error::NotFound)?;
        if u64::try_from(timeline.len()).map_err(|_| Error::TooLarge)? != change.expected_revision {
            return Err(Error::Conflict);
        }
        match change.kind {
            ChangeKind::Restore if !previous.deleted => return Err(Error::Conflict),
            ChangeKind::Update | ChangeKind::Delete if previous.deleted => {
                return Err(Error::Conflict);
            }
            _ => (),
        }
        if change.kind == ChangeKind::Delete {
            if self.metadata_schema(previous) != Some(record.schema) || !record.payload.is_empty() {
                return Err(invalid(
                    "tombstone must preserve schema and have empty payload",
                ));
            }
        } else if !pruned {
            self.decode_values(record.schema, &record.payload, 0)?;
        }
        Ok(())
    }
    fn schema_depth(&self, sr: SchemaRef, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(invalid("schema depth exceeded"));
        }
        let schema = self.schemas.get(&sr).ok_or(Error::UnknownSchema)?;
        for field in &schema.fields {
            if let FieldType::Struct(child) = field.kind {
                self.schema_depth(child, depth + 1)?;
            }
        }
        Ok(())
    }
    fn apply(
        &mut self,
        event: Event,
        sequence: u64,
        offset: Option<u64>,
        source: Option<Arc<Entry>>,
        bytes: &[u8],
    ) -> Result<()> {
        self.validate_schema_bindings(&event)?;
        self.validate_member_locations(&event, sequence, offset)?;
        self.validate_frame_source(sequence, offset, source.as_ref(), bytes)?;
        let ranges = history_read::payload_ranges(bytes, &event)?;
        let prepared_fingerprint = match &event {
            Event::Transaction(tx) => {
                Some(profile_memory!(RegistryReceipts, fingerprint(&tx.records))?)
            }
            Event::Changes(tx) => {
                Some(profile_memory!(RegistryReceipts, fingerprint(&tx.changes))?)
            }
            _ => None,
        };
        self.prepare_metadata_growth(&event)?;
        profile_memory!(
            RegistryHistoryMetadata,
            self.frames.push(FrameSource {
                offset: offset.unwrap_or(0),
                entry: source
            })
        );
        match event {
            Event::Namespace(ns) => {
                profile_memory!(RegistryDefinitions, self.namespaces.insert(ns.id, ns));
            }
            Event::Concept(c) => {
                profile_memory!(
                    RegistryDefinitions,
                    self.concepts.insert((c.namespace, c.id, c.version), c)
                );
            }
            Event::Schema(s) => {
                let ordinal = self.schema_table.len();
                profile_memory!(RegistryHistoryMetadata, self.schema_table.push(s.reference));
                profile_memory!(
                    RegistryHistoryMetadata,
                    self.schema_ordinals.insert(s.reference, ordinal)
                );
                profile_memory!(RegistryDefinitions, self.schemas.insert(s.reference, s));
            }
            Event::RelationRule(rule) => {
                profile_memory!(
                    RegistryDefinitions,
                    self.relation_rules.insert(rule.schema, rule)
                );
            }
            Event::Record(r) => {
                self.apply_revision(r, false, sequence, ranges[0], 0);
            }
            Event::Transaction(tx) => {
                let receipt = TransactionReceipt {
                    tx_id: tx.id,
                    commit_sequence: sequence,
                    records: tx.records.len(),
                };
                for (member, record) in tx.records.into_iter().enumerate() {
                    self.apply_revision(record, false, sequence, ranges[member], member as u16);
                }
                profile_memory!(
                    RegistryReceipts,
                    self.transactions.insert(
                        tx.id,
                        (
                            receipt,
                            prepared_fingerprint.expect("prepared transaction fingerprint"),
                        ),
                    )
                );
            }
            Event::Changes(tx) => {
                let receipt = TransactionReceipt {
                    tx_id: tx.id,
                    commit_sequence: sequence,
                    records: tx.changes.len(),
                };
                for (member, change) in tx.changes.into_iter().enumerate() {
                    self.apply_revision(
                        change.record,
                        change.kind == ChangeKind::Delete,
                        sequence,
                        ranges[member],
                        member as u16,
                    );
                }
                profile_memory!(
                    RegistryReceipts,
                    self.change_transactions.insert(
                        tx.id,
                        (
                            receipt,
                            prepared_fingerprint.expect("prepared changes fingerprint"),
                        ),
                    )
                );
            }
            Event::Revision(change) => self.apply_revision(
                change.record,
                change.kind == ChangeKind::Delete,
                sequence,
                ranges[0],
                0,
            ),
            Event::Compacted(tx) => {
                let receipt = tx.tx_id.map(|id| TransactionReceipt {
                    tx_id: id,
                    commit_sequence: sequence,
                    records: tx.members.len(),
                });
                for (ordinal, member) in tx.members.into_iter().enumerate() {
                    self.apply_revision_inner(
                        member.change.record,
                        member.change.kind == ChangeKind::Delete,
                        sequence,
                        member.pruned,
                        ranges[ordinal],
                        ordinal as u16,
                    );
                }
                if let (Some(receipt), Some(hash)) = (receipt, tx.fingerprint) {
                    match tx.kind {
                        PrunedKind::Transaction => {
                            profile_memory!(
                                RegistryReceipts,
                                self.transactions.insert(receipt.tx_id, (receipt, hash))
                            );
                        }
                        PrunedKind::Changes => {
                            profile_memory!(
                                RegistryReceipts,
                                self.change_transactions
                                    .insert(receipt.tx_id, (receipt, hash))
                            );
                        }
                        _ => (),
                    }
                }
            }
        }
        Ok(())
    }
    fn apply_revision(
        &mut self,
        record: Record,
        deleted: bool,
        sequence: u64,
        range: PayloadRange,
        member: u16,
    ) {
        self.apply_revision_inner(record, deleted, sequence, false, range, member);
    }
    fn apply_revision_inner(
        &mut self,
        record: Record,
        deleted: bool,
        sequence: u64,
        pruned: bool,
        range: PayloadRange,
        member: u16,
    ) {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::RegistryHistory);
        let key = (record.namespace, record.id);
        let reference = Reference {
            namespace: record.namespace,
            id: record.id,
        };
        if let Some(old) = self.native_edges.remove(&reference) {
            for (index, endpoint) in [
                (&mut self.by_source, old.source),
                (&mut self.by_target, old.target),
            ] {
                if let Some(set) = index.get_mut(&endpoint) {
                    set.remove(&reference);
                    if set.is_empty() {
                        index.remove(&endpoint);
                    }
                }
            }
        }
        if !deleted
            && let Some(edge) = profile_memory!(
                RegistryDecode,
                self.native_from_record(&record, sequence)
                    .expect("validated native edge")
            )
        {
            profile_memory!(
                RegistryRelations,
                self.by_source
                    .entry(edge.source)
                    .or_default()
                    .insert(reference)
            );
            profile_memory!(
                RegistryRelations,
                self.by_target
                    .entry(edge.target)
                    .or_default()
                    .insert(reference)
            );
            profile_memory!(RegistryRelations, self.native_edges.insert(reference, edge));
        }
        if deleted {
            profile_memory!(
                RegistryCurrent,
                Arc::make_mut(&mut self.records).remove(&key)
            );
        } else {
            profile_memory!(
                RegistryCurrent,
                Arc::make_mut(&mut self.records).insert(key, record.clone())
            );
        }
        profile_memory!(
            RegistryHistoryMetadata,
            self.history_meta.entry(key).or_default().push(HistoryMeta {
                schema_ordinal: self.schema_ordinals[&record.schema],
                payload_start: range.start,
                payload_len: range.len,
                member,
                deleted,
                commit_sequence: sequence,
            })
        );
        self.invalidate_public_history(key);
        if pruned {
            self.pruned.insert((key.0, key.1, record.revision));
        }
        #[cfg(feature = "memory-profile")]
        if !pruned {
            self.history_cache_stats
                .get_mut()
                .unwrap_or_else(|e| e.into_inner())
                .logical_available_items += 1;
        }
    }

    fn encode_values(
        &self,
        sr: SchemaRef,
        values: &[Option<Value>],
        depth: usize,
    ) -> Result<Vec<u8>> {
        if depth > MAX_DEPTH {
            return Err(invalid("value depth exceeded"));
        }
        let schema = self.schemas.get(&sr).ok_or(Error::UnknownSchema)?;
        if schema.fields.len() != values.len() {
            return Err(invalid("field cardinality mismatch"));
        }
        let mut bitmap = vec![
            0u8;
            schema
                .fields
                .iter()
                .filter(|f| f.optional)
                .count()
                .div_ceil(8)
        ];
        let mut optional = 0;
        let mut present = Vec::new();
        for (field, value) in schema.fields.iter().zip(values) {
            if field.optional {
                if value.is_some() {
                    bitmap[optional / 8] |= 1 << (optional % 8);
                }
                optional += 1;
            } else if value.is_none() {
                return Err(invalid("required field absent"));
            }
            if let Some(v) = value {
                self.validate_value(&field.kind, v, depth)?;
                present.push(v.clone());
            }
        }
        canonical_codec::encode(&Payload {
            bitmap,
            values: present,
        })
    }
    fn decode_values(
        &self,
        sr: SchemaRef,
        bytes: &[u8],
        depth: usize,
    ) -> Result<Vec<Option<Value>>> {
        if depth > MAX_DEPTH {
            return Err(invalid("value depth exceeded"));
        }
        let schema = self.schemas.get(&sr).ok_or(Error::UnknownSchema)?;
        let payload: Payload = canonical_codec::decode(bytes)?;
        let optional_count = schema.fields.iter().filter(|f| f.optional).count();
        if payload.bitmap.len() != optional_count.div_ceil(8) {
            return Err(invalid("bitmap length mismatch"));
        }
        if !optional_count.is_multiple_of(8)
            && payload
                .bitmap
                .last()
                .is_some_and(|b| *b >> (optional_count % 8) != 0)
        {
            return Err(invalid("bitmap reserved bits"));
        }
        if payload.values.len() > MAX_FIELDS {
            return Err(invalid("too many values"));
        }
        let mut values = payload.values.into_iter();
        let mut out = Vec::with_capacity(schema.fields.len());
        let mut optional = 0;
        for field in &schema.fields {
            let present = if field.optional {
                let bit = payload.bitmap[optional / 8] & (1 << (optional % 8)) != 0;
                optional += 1;
                bit
            } else {
                true
            };
            if present {
                let value = values.next().ok_or(invalid("missing positional value"))?;
                self.validate_value(&field.kind, &value, depth)?;
                out.push(Some(value));
            } else {
                out.push(None);
            }
        }
        if values.next().is_some() {
            return Err(invalid("excess positional value"));
        }
        Ok(out)
    }
    fn validate_value(&self, kind: &FieldType, value: &Value, depth: usize) -> Result<()> {
        let valid = match (kind, value) {
            (FieldType::Bool, Value::Bool(_))
            | (FieldType::UInt, Value::UInt(_))
            | (FieldType::Int, Value::Int(_))
            | (FieldType::FixedInt, Value::FixedInt(_)) => true,
            (FieldType::Decimal { scale: a }, Value::Decimal { scale: b, .. }) => a == b,
            (FieldType::Float, Value::Float(f)) => f.is_finite(),
            (FieldType::Enum { variants }, Value::Enum(v)) => variants.contains(v),
            (FieldType::Utf8, Value::Utf8(s)) => s.len() <= MAX_TEXT_BYTES,
            (FieldType::Bytes, Value::Bytes(b)) => b.len() <= MAX_TEXT_BYTES,
            (FieldType::Ref, Value::Ref(r)) => {
                reference(*r)?;
                self.require_namespace(r.namespace)?;
                true
            }
            (FieldType::Struct(sr), Value::Struct(b)) => {
                self.decode_values(*sr, b, depth + 1)?;
                true
            }
            (
                FieldType::Relation,
                Value::Relation {
                    source,
                    kind,
                    target,
                },
            ) => {
                reference(*source)?;
                reference(*target)?;
                self.require_namespace(source.namespace)?;
                self.require_namespace(target.namespace)?;
                *kind != 0 // Endpoint existence and relation authorization are E07.
            }
            (FieldType::Array { element, max_items }, Value::Array(items)) => {
                if items.len() > *max_items {
                    return Err(invalid("array limit exceeded"));
                }
                for item in items {
                    self.validate_scalar(element, item)?;
                }
                true
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid("field type or value invalid"))
        }
    }
    fn validate_scalar(&self, kind: &ScalarType, value: &ScalarValue) -> Result<()> {
        let valid = match (kind, value) {
            (ScalarType::Bool, ScalarValue::Bool(_))
            | (ScalarType::UInt, ScalarValue::UInt(_))
            | (ScalarType::Int, ScalarValue::Int(_)) => true,
            (ScalarType::Utf8, ScalarValue::Utf8(s)) => s.len() <= MAX_TEXT_BYTES,
            (ScalarType::Bytes, ScalarValue::Bytes(b)) => b.len() <= MAX_TEXT_BYTES,
            (ScalarType::Ref, ScalarValue::Ref(r)) => {
                reference(*r)?;
                self.require_namespace(r.namespace)?;
                true
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(invalid("array scalar type invalid"))
        }
    }
}

fn fingerprint<T: Serialize>(value: &T) -> Result<[u8; 32]> {
    Ok(Sha256::digest(canonical_codec::encode(value)?).into())
}

#[cfg(test)]
mod history_metadata_e15_tests {
    use super::*;
    #[test]
    fn metadata_never_retains_a_payload_allocation_while_public_history_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registry::open(dir.path()).unwrap();
        r.register_namespace(Namespace {
            id: 0,
            name: "metadata".into(),
            level: Level::Global,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 0,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        let rec = r
            .create(0, 1, sr, &[Some(Value::Bytes(vec![9; 4096]))])
            .unwrap();
        let meta = &r.state.history_meta[&(0, 1)][0];
        assert_eq!(r.state.metadata_schema(meta), Some(rec.schema));
        assert_eq!(meta.member, 0);
        assert!(
            r.state
                .canonical_location(0, 1, Some(1))
                .unwrap()
                .frame_offset
                >= 16
        );
        assert!(!meta.deleted);
        assert_eq!(meta.commit_sequence, r.history(0, 1)[0].commit_sequence);
        assert_eq!(r.history(0, 1)[0].record, rec);
    }
}

#[cfg(test)]
mod history_metadata_e17_tests {
    use super::*;
    #[test]
    fn private_timeline_entry_is_smaller_than_public_revision_without_changing_payload_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registry::open(dir.path()).unwrap();
        r.register_namespace(Namespace {
            id: 0,
            name: "compact-meta".into(),
            level: Level::Global,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 0,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "data".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        let record = r
            .create(0, 1, sr, &[Some(Value::Bytes(vec![7; 4096]))])
            .unwrap();
        assert_eq!(r.history(0, 1)[0].record, record);
        let meta = &r.state.history_meta[&(0, 1)][0];
        eprintln!(
            "E17 inline sizes: HistoryMeta={} Revision={} Record={}",
            std::mem::size_of_val(meta),
            std::mem::size_of::<Revision>(),
            std::mem::size_of::<Record>()
        );
        assert!(
            std::mem::size_of_val(meta) < std::mem::size_of::<Revision>(),
            "private={} public={}",
            std::mem::size_of_val(meta),
            std::mem::size_of::<Revision>()
        );
    }
}

#[cfg(test)]
mod canonical_timeline_e18_tests {
    use super::*;
    fn contiguous(r: &Registry) {
        let mut expected = PrimaryIndex::new();
        let mut slots = BTreeMap::<(u64, u64), u64>::new();
        for entry in r.journal.snapshot() {
            let envelope: Envelope = canonical_codec::decode(&entry.payload).unwrap();
            let offset = r.journal.frame_offset(entry.sequence).unwrap();
            index_event(&mut expected, &envelope.event, entry.sequence, offset).unwrap();
            for (member, (record, deleted)) in
                event_members(&envelope.event).into_iter().enumerate()
            {
                let ordinal = slots.entry((record.namespace, record.id)).or_default();
                *ordinal += 1;
                assert_eq!(record.revision, *ordinal);
                let meta =
                    &r.state.history_meta[&(record.namespace, record.id)][(*ordinal - 1) as usize];
                assert_eq!(r.state.metadata_schema(meta), Some(record.schema));
                assert_eq!(meta.deleted, deleted);
                assert_eq!(meta.commit_sequence, entry.sequence);
                assert_eq!(
                    r.state
                        .canonical_location(record.namespace, record.id, Some(record.revision))
                        .unwrap()
                        .frame_offset,
                    offset
                );
                assert_eq!(meta.member as usize, member);
            }
        }
        assert_eq!(
            r.state
                .canonical_entries()
                .collect::<Result<Vec<_>>>()
                .unwrap(),
            expected.entries()
        );
        assert_eq!(
            r.state.canonical_entry_count().unwrap(),
            expected.len() as u64
        );
        for (key, timeline) in &r.state.history_meta {
            assert_eq!(timeline.len() as u64, slots[key]);
        }
        assert_eq!(std::mem::size_of::<HistoryMeta>(), 32);
        for format in [
            CacheFormat::Legacy,
            CacheFormat::Grouped,
            CacheFormat::Delta,
        ] {
            let actual = r
                .index_path
                .with_file_name(format!("canonical-{format:?}.cbi"));
            let old = r.index_path.with_file_name(format!("old-{format:?}.cbi"));
            std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
            save_entries_with_format(
                &actual,
                r.sequence,
                r.state.canonical_entry_count().unwrap(),
                format,
                r.state.canonical_entries(),
            )
            .unwrap();
            expected.save_with_format(&old, r.sequence, format).unwrap();
            assert_eq!(
                std::fs::read(&actual).unwrap(),
                std::fs::read(&old).unwrap()
            );
            assert_eq!(
                validate_cache_entries(
                    &actual,
                    r.sequence,
                    r.state.canonical_entry_count().unwrap(),
                    r.state.canonical_entries()
                )
                .unwrap(),
                CacheValidation::Match
            );
        }
    }
    #[test]
    fn all_event_shapes_and_pruned_rewrites_keep_every_revision_slot() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Registry::open(d.path()).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "timeline".into(),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![],
        })
        .unwrap();
        let input = |id| NewRecord {
            namespace: 1,
            id,
            schema: sr,
            values: vec![],
        };
        r.create(1, 1, sr, &[]).unwrap();
        contiguous(&r);
        r.create_batch([1; 16], &[input(2), input(3)]).unwrap();
        contiguous(&r);
        r.update(input(1), 1).unwrap();
        contiguous(&r);
        r.commit(
            [2; 16],
            &[
                Mutation::Update {
                    record: input(2),
                    expected_revision: 1,
                },
                Mutation::Delete {
                    namespace: 1,
                    id: 3,
                    expected_revision: 1,
                },
            ],
        )
        .unwrap();
        contiguous(&r);
        r.delete(1, 1, 2).unwrap();
        contiguous(&r);
        r.restore(input(1), 3).unwrap();
        contiguous(&r);
        r.restore(input(3), 2).unwrap();
        contiguous(&r);
        r.compact(Retention::KeepLast(1)).unwrap();
        contiguous(&r);
        assert!(!r.state.pruned.is_empty());
        r.compact(Retention::KeepAll).unwrap();
        contiguous(&r);
        drop(r);
        let mut r = Registry::open(d.path()).unwrap();
        contiguous(&r);
        r.update(input(1), 4).unwrap();
        contiguous(&r);
        r.compact(Retention::KeepLast(1)).unwrap();
        contiguous(&r);
        assert_eq!(r.state.history_meta[&(1, 1)].len(), 5);
        drop(r);
        contiguous(&Registry::open(d.path()).unwrap());
    }
}

#[cfg(test)]
mod schema_reference_e22_tests {
    use super::*;
    #[test]
    fn private_history_metadata_uses_a_32_byte_slot_without_changing_public_records() {
        assert_eq!(
            std::mem::size_of::<HistoryMeta>(),
            32,
            "actual private metadata size must reduce from E21 48 bytes"
        );
        assert_eq!(std::mem::size_of::<SchemaRef>(), 24);
        assert_eq!(std::mem::size_of::<Record>(), 72);
        assert_eq!(std::mem::size_of::<Revision>(), 88);
    }
}

#[cfg(test)]
mod replay_capacity_e23_tests {
    use super::*;
    #[test]
    fn replay_removes_unused_private_metadata_capacity_without_pruning_slots() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Registry::open(d.path()).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "capacity".into(),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![],
        })
        .unwrap();
        r.create(1, 1, sr, &[]).unwrap();
        for expected in 1..11 {
            r.update(
                NewRecord {
                    namespace: 1,
                    id: 1,
                    schema: sr,
                    values: vec![],
                },
                expected,
            )
            .unwrap();
        }
        drop(r);
        let r = Registry::open(d.path()).unwrap();
        let timeline = &r.state.history_meta[&(1, 1)];
        assert_eq!(timeline.len(), 11);
        assert_eq!(
            timeline.capacity(),
            11,
            "replayed capacity must not retain the E22 16-slot buffer"
        );
        assert_eq!(r.history(1, 1).len(), 11);
    }
}

#[cfg(test)]
mod shared_current_snapshot_e24_tests {
    use super::*;
    #[test]
    fn current_snapshot_shares_the_canonical_record_and_payload_addresses() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Registry::open(d.path()).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "sharing".into(),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        r.create(1, 1, sr, &[Some(Value::Bytes(vec![7; 4096]))])
            .unwrap();
        let snapshot = r.snapshot();
        assert!(Arc::ptr_eq(&snapshot.records, &snapshot._reader.records));
        assert!(
            std::ptr::eq(r.record(1, 1).unwrap(), snapshot.record(1, 1).unwrap()),
            "current snapshot must share the canonical BTreeMap instead of cloning Records"
        );
        assert_eq!(
            r.record(1, 1).unwrap().payload.as_ptr(),
            snapshot.record(1, 1).unwrap().payload.as_ptr()
        );
    }
}

#[cfg(test)]
mod reader_leases_e24_tests {
    use super::*;
    fn setup(path: &Path) -> (Registry, SchemaRef) {
        let mut r = Registry::open(path).unwrap();
        r.register_namespace(Namespace {
            id: 1,
            name: "readers".into(),
            level: Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![Field {
                label: "bytes".into(),
                kind: FieldType::Bytes,
                optional: false,
            }],
        })
        .unwrap();
        for id in [1, 2] {
            r.create(1, id, sr, &[Some(Value::Bytes(vec![id as u8; 4096]))])
                .unwrap();
        }
        (r, sr)
    }
    fn readers(r: &Registry) -> usize {
        r.snapshots
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count()
    }
    #[test]
    fn state_ownership_never_counts_as_a_reader_and_independent_leases_survive_clones() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Snapshot>();
        let d = tempfile::tempdir().unwrap();
        let (r, _) = setup(d.path());
        let first = r.snapshot();
        let cloned = first.clone();
        let second = r.snapshot();
        assert!(Arc::ptr_eq(&first._reader.records, &r.state.records));
        assert!(Arc::ptr_eq(&first._reader.records, &second._reader.records));
        assert!(!Arc::ptr_eq(&first._reader, &second._reader));
        assert!(Arc::ptr_eq(&first._reader, &cloned._reader));
        assert_eq!(readers(&r), 2);
        drop(first);
        assert_eq!(readers(&r), 2);
        drop(cloned);
        assert_eq!(readers(&r), 1);
        drop(second);
        assert_eq!(readers(&r), 0);
        assert_eq!(Arc::strong_count(&r.state.records), 1);
        assert_eq!(r.record(1, 1).unwrap().revision, 1);
    }
    #[test]
    fn cow_detaches_only_first_write_and_preserves_all_old_snapshots_and_pin_retention() {
        let d = tempfile::tempdir().unwrap();
        let (mut r, sr) = setup(d.path());
        let first = r.snapshot();
        let clone = first.clone();
        let initial = first.record(1, 1).unwrap().clone();
        let input = |v| NewRecord {
            namespace: 1,
            id: 1,
            schema: sr,
            values: vec![Some(Value::Bytes(vec![v; 31]))],
        };
        r.update(input(3), 1).unwrap();
        assert_eq!(first.record(1, 1), Some(&initial));
        assert!(!Arc::ptr_eq(&r.state.records, &first._reader.records));
        let detached = Arc::as_ptr(&r.state.records);
        r.update(input(4), 2).unwrap();
        assert_eq!(Arc::as_ptr(&r.state.records), detached);
        assert_eq!(clone.record(1, 1), Some(&initial));
        drop(first);
        r.compact(Retention::KeepLast(1)).unwrap();
        assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Available);
        drop(clone);
        r.compact(Retention::KeepLast(1)).unwrap();
        assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Pruned);
        let current = r.snapshot();
        r.commit(
            [8; 16],
            &[
                Mutation::Update {
                    record: input(5),
                    expected_revision: 3,
                },
                Mutation::Delete {
                    namespace: 1,
                    id: 2,
                    expected_revision: 1,
                },
            ],
        )
        .unwrap();
        assert_eq!(current.record(1, 1).unwrap().revision, 3);
        assert!(current.record(1, 2).is_some());
        assert!(r.record(1, 2).is_none());
    }
    #[test]
    fn historical_and_current_snapshots_keep_prefixes_through_late_generation_failure_and_reopen() {
        for stage in [
            canonical_storage::RewriteStage::ManifestPublished,
            canonical_storage::RewriteStage::Activated,
        ] {
            let d = tempfile::tempdir().unwrap();
            let (mut r, sr) = setup(d.path());
            let current = r.snapshot();
            let historical = r.snapshot_at(current.commit_sequence).unwrap();
            let initial = current.record(1, 1).unwrap().clone();
            r.update(
                NewRecord {
                    namespace: 1,
                    id: 1,
                    schema: sr,
                    values: vec![Some(Value::Bytes(vec![9; 17]))],
                },
                1,
            )
            .unwrap();
            assert!(
                r.compact_with_hook(Retention::KeepLast(1), |s| if s == stage {
                    Err(invalid("reader generation failure"))
                } else {
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(current.record(1, 1), Some(&initial));
            assert_eq!(historical.record(1, 1), Some(&initial));
            drop(r);
            let mut r = Registry::open(d.path()).unwrap();
            r.compact(Retention::KeepLast(1)).unwrap();
            assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Available);
            drop(current);
            drop(historical);
            r.compact(Retention::KeepLast(1)).unwrap();
            assert_eq!(r.revision_status(1, 1, 1), RevisionStatus::Pruned);
        }
    }
}
