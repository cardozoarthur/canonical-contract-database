//! Typed deterministic queries over one immutable snapshot. Indexes are derived in memory.
//! Cursor tokens identify bounded local session state; they never grant authorization.
use canonical_core::{Error, Result};
#[cfg(test)]
use canonical_registry::Schema;
use canonical_registry::{
    FieldType, ProjectionDecoder, Record, Registry, SchemaRef, Snapshot, Value,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

// Opt-in allocation origins only; default expansion preserves the original expression.
macro_rules! profile_memory {
    ($component:ident, $expression:expr) => {{
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::$component);
        $expression
    }};
}

#[cfg(feature = "memory-profile")]
mod memory;

pub const MAX_QUERY_DEPTH: usize = 16;
pub const MAX_QUERY_NODES: usize = 256;
pub const MAX_QUERY_LITERAL_BYTES: usize = 64 * 1024;
pub const MAX_PAGE: usize = 1000;
pub const MAX_CURSORS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FieldRef {
    pub schema: SchemaRef,
    pub position: usize,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Predicate {
    True,
    Eq(FieldRef, Value),
    Range {
        field: FieldRef,
        lower: Option<Value>,
        upper: Option<Value>,
    },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    pub id: String,
    pub readable_namespaces: BTreeSet<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Order {
    pub field: Option<FieldRef>,
    pub descending: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub namespaces: Vec<u64>,
    pub schemas: Vec<SchemaRef>,
    pub filter: Predicate,
    pub order: Order,
    pub limit: usize,
    pub cursor: Option<Cursor>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor([u8; 24]);
impl Cursor {
    pub fn to_bytes(&self) -> [u8; 24] {
        self.0
    }
    pub fn from_bytes(bytes: [u8; 24]) -> Self {
        Self(bytes)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStrategy {
    Indexed,
    Scan,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryPlan {
    pub strategy: PlanStrategy,
    pub index_lookups: u64,
    pub candidates: usize,
    pub rows_examined: usize,
    pub snapshot_sequence: u64,
}
#[derive(Clone, Debug)]
pub struct Page {
    pub records: Vec<Record>,
    pub next: Option<Cursor>,
    pub plan: QueryPlan,
}

/// Identity metadata only: no payload is copied or returned by query_ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdRow {
    pub namespace: u64,
    pub id: u64,
    pub revision: u64,
    pub schema: SchemaRef,
}
#[derive(Clone, Debug)]
pub struct IdPage {
    pub ids: Vec<IdRow>,
    pub next: Option<Cursor>,
    pub plan: QueryPlan,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedRow {
    pub identity: IdRow,
    pub fields: Vec<Option<Value>>,
}
#[derive(Clone, Debug)]
pub struct ProjectedPage {
    pub rows: Vec<ProjectedRow>,
    pub next: Option<Cursor>,
    pub plan: QueryPlan,
}
type Projection = BTreeMap<SchemaRef, Vec<usize>>;
#[derive(Clone, Debug, PartialEq, Eq)]
enum OutputShape {
    Records,
    Ids,
    Projected(Projection),
}
#[derive(Clone, Copy)]
enum ShapeRef<'a> {
    Records,
    Ids,
    Projected(&'a Projection),
}
impl ShapeRef<'_> {
    fn matches(self, owned: &OutputShape) -> bool {
        match (self, owned) {
            (Self::Records, OutputShape::Records) | (Self::Ids, OutputShape::Ids) => true,
            (Self::Projected(a), OutputShape::Projected(b)) => a == b,
            _ => false,
        }
    }
    fn owned(self) -> OutputShape {
        match self {
            Self::Records => OutputShape::Records,
            Self::Ids => OutputShape::Ids,
            Self::Projected(p) => OutputShape::Projected(p.clone()),
        }
    }
}
struct Selection {
    keys: Vec<usize>,
    start: usize,
    end: usize,
    next: Option<Cursor>,
    plan: QueryPlan,
}

type RowKey = (u64, u64, u64);
#[cfg(test)]
type KeySet = BTreeSet<RowKey>;
type Secondary = BTreeMap<FieldRef, Box<[usize]>>;
struct DenseRow {
    key: RowKey,
    row: Row,
}
#[cfg(test)]
mod legacy;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Scalar {
    Bool(bool),
    UInt(u64),
    Int(i64),
    FixedInt(i64),
    Decimal(i64, u8),
    Float(u64),
    Enum(u32),
    Ref(u64, u64),
    Utf8(String),
    Bytes(Vec<u8>),
}
struct Row {
    schema: SchemaRef,
    values: Vec<Option<Scalar>>,
    // Native edge visibility belongs to this record revision, not today's graph.
    // Ordinary/legacy records repeat their own namespace and keep prior semantics.
    required_namespaces: [u64; 3],
}
struct Binding {
    subject: Subject,
    query: Query,
    shape: OutputShape,
}
impl Binding {
    fn matches(&self, subject: &Subject, query: &Query, shape: ShapeRef<'_>) -> bool {
        shape.matches(&self.shape)
            && self.subject == *subject
            && self.query.namespaces == query.namespaces
            && self.query.schemas == query.schemas
            && self.query.filter == query.filter
            && self.query.order == query.order
            && self.query.limit == query.limit
    }
}
struct CursorState {
    binding: Arc<Binding>,
    start: usize,
}

pub struct QueryEngine {
    sequence: u64,
    owner: [u8; 16],
    next_slot: u64,
    rows: Box<[DenseRow]>,
    decoder: ProjectionDecoder,
    snapshot: Snapshot,
    by_namespace: BTreeMap<u64, Box<[usize]>>,
    by_schema: BTreeMap<SchemaRef, Box<[usize]>>,
    secondary: Secondary,
    cursors: BTreeMap<u64, CursorState>,
    #[cfg(test)]
    legacy: legacy::LegacyIndex,
}

impl QueryEngine {
    pub fn new(registry: &Registry, snapshot: Snapshot) -> Result<Self> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::QueryTemporary);
        // Public owner identity, not a capability or signing key. Do not initialize a
        // session if the OS random source fails; never fall back to a reused counter.
        let owner = session_owner(getrandom::fill)?;
        #[cfg(test)]
        let legacy = profile_memory!(Harness, legacy::LegacyIndex::new(registry, &snapshot)?);
        let mut engine = Self {
            sequence: snapshot.commit_sequence,
            owner,
            next_slot: 1,
            rows: Box::default(),
            decoder: profile_memory!(QueryRows, registry.projection_decoder()),
            snapshot,
            by_namespace: BTreeMap::new(),
            by_schema: BTreeMap::new(),
            secondary: BTreeMap::new(),
            cursors: BTreeMap::new(),
            #[cfg(test)]
            legacy,
        };
        let mut rows = profile_memory!(QueryRows, Vec::with_capacity(engine.snapshot.len()));
        for record in engine.snapshot.iter() {
            let schema = registry.schema(record.schema)?;
            let values = registry.decode_values(record.schema, &record.payload)?;
            let required_namespaces = if let Some(rule) = registry.relation_rule(record.schema) {
                let [
                    Some(Value::Relation {
                        source,
                        kind,
                        target,
                    }),
                ] = values.as_slice()
                else {
                    return Err(invalid("native relation snapshot payload is invalid"));
                };
                if *kind != rule.kind
                    || engine
                        .snapshot
                        .record(source.namespace, source.id)
                        .map(|r| r.schema)
                        != Some(rule.source_schema)
                    || engine
                        .snapshot
                        .record(target.namespace, target.id)
                        .map(|r| r.schema)
                        != Some(rule.target_schema)
                {
                    return Err(invalid(
                        "native relation snapshot violates endpoint contract",
                    ));
                }
                [record.namespace, source.namespace, target.namespace]
            } else {
                [record.namespace; 3]
            };
            let mut scalars = profile_memory!(QueryScalars, Vec::with_capacity(values.len()));
            let key = (record.namespace, record.id, record.revision);
            for (field, value) in schema.fields.iter().zip(values) {
                let scalar = if supported(&field.kind) {
                    profile_memory!(
                        QueryScalars,
                        value.as_ref().map(|v| scalar(&field.kind, v)).transpose()?
                    )
                } else {
                    None
                };
                scalars.push(scalar);
            }
            profile_memory!(
                QueryRows,
                rows.push(DenseRow {
                    key,
                    row: Row {
                        schema: record.schema,
                        values: scalars,
                        required_namespaces
                    }
                })
            );
        }
        rows.sort_unstable_by_key(|entry| entry.key);
        engine.rows = profile_memory!(QueryRows, rows.into_boxed_slice());
        {
            #[cfg(feature = "memory-profile")]
            let _index_origin = canonical_memory::scope(canonical_memory::Component::QueryIndexes);
            let mut namespaces: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
            let mut schemas: BTreeMap<SchemaRef, Vec<usize>> = BTreeMap::new();
            let mut fields: BTreeMap<FieldRef, Vec<usize>> = BTreeMap::new();
            for (ordinal, entry) in engine.rows.iter().enumerate() {
                namespaces.entry(entry.key.0).or_default().push(ordinal);
                schemas.entry(entry.row.schema).or_default().push(ordinal);
                for (position, value) in entry.row.values.iter().enumerate() {
                    if value.is_some() {
                        fields
                            .entry(FieldRef {
                                schema: entry.row.schema,
                                position,
                            })
                            .or_default()
                            .push(ordinal);
                    }
                }
            }
            engine.by_namespace = namespaces
                .into_iter()
                .map(|(key, v)| (key, v.into_boxed_slice()))
                .collect();
            engine.by_schema = schemas
                .into_iter()
                .map(|(key, v)| (key, v.into_boxed_slice()))
                .collect();
            engine.secondary = fields
                .into_iter()
                .map(|(field, mut ordinals)| {
                    ordinals.sort_unstable_by(|&a, &b| {
                        engine.rows[a].row.values[field.position]
                            .cmp(&engine.rows[b].row.values[field.position])
                            .then(a.cmp(&b))
                    });
                    (field, ordinals.into_boxed_slice())
                })
                .collect();
        }
        #[cfg(feature = "memory-profile")]
        memory::publish_query(&engine);
        Ok(engine)
    }

    pub fn query(&mut self, subject: &Subject, query: &Query) -> Result<Page> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::QueryTemporary);
        let selected = self.select(subject, query, ShapeRef::Records)?;
        let records = selected.keys[selected.start..selected.end]
            .iter()
            .map(|key| {
                self.snapshot
                    .record(self.rows[*key].key.0, self.rows[*key].key.1)
                    .expect("validated snapshot key")
                    .clone()
            })
            .collect();
        Ok(Page {
            records,
            next: selected.next,
            plan: selected.plan,
        })
    }
    pub fn query_ids(&mut self, subject: &Subject, query: &Query) -> Result<IdPage> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::QueryTemporary);
        let selected = self.select(subject, query, ShapeRef::Ids)?;
        let ids = selected.keys[selected.start..selected.end]
            .iter()
            .map(|key| self.identity(*key))
            .collect();
        Ok(IdPage {
            ids,
            next: selected.next,
            plan: selected.plan,
        })
    }
    pub fn query_projected(
        &mut self,
        subject: &Subject,
        query: &Query,
        projection: &BTreeMap<SchemaRef, Vec<usize>>,
    ) -> Result<ProjectedPage> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::QueryTemporary);
        let selected = self.select(subject, query, ShapeRef::Projected(projection))?;
        let rows = selected.keys[selected.start..selected.end]
            .iter()
            .map(|key| {
                let record = self
                    .snapshot
                    .record(self.rows[*key].key.0, self.rows[*key].key.1)
                    .expect("validated snapshot key");
                let fields = self.decoder.decode_selected(
                    record.schema,
                    &record.payload,
                    &projection[&record.schema],
                )?;
                Ok(ProjectedRow {
                    identity: self.identity(*key),
                    fields,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ProjectedPage {
            rows,
            next: selected.next,
            plan: selected.plan,
        })
    }
    fn identity(&self, ordinal: usize) -> IdRow {
        let key = self.rows[ordinal].key;
        IdRow {
            namespace: key.0,
            id: key.1,
            revision: key.2,
            schema: self.rows[ordinal].row.schema,
        }
    }
    fn select(
        &mut self,
        subject: &Subject,
        query: &Query,
        shape: ShapeRef<'_>,
    ) -> Result<Selection> {
        // Authorization precedes schema/type/cursor validation, and is repeated for every page.
        if subject.id.is_empty() || query.namespaces.is_empty() {
            return Err(Error::Unauthorized);
        }
        if subject.id.len() > 1024
            || subject.readable_namespaces.len() > 4096
            || query.namespaces.len() > 256
            || query.schemas.len() > 256
        {
            return Err(Error::TooLarge);
        }
        if query
            .namespaces
            .iter()
            .any(|ns| !subject.readable_namespaces.contains(ns))
        {
            return Err(Error::Unauthorized);
        }
        if query.limit == 0 || query.limit > MAX_PAGE {
            return Err(invalid("page limit must be 1..1000"));
        }
        for schema in &query.schemas {
            self.decoder.schema(*schema)?;
        }
        let mut budget = Budget::default();
        self.validate(&query.filter, 1, &mut budget)?;
        if let Some(field) = query.order.field {
            self.field_kind(field)?;
        }
        if let ShapeRef::Projected(projection) = shape {
            self.validate_projection(subject, query, projection)?;
        }
        let (start, binding) = match &query.cursor {
            Some(cursor) => {
                let slot = u64::from_le_bytes(cursor.0[16..].try_into().unwrap());
                if cursor.0[..16] != self.owner {
                    return Err(Error::InvalidCursor);
                }
                let state = self.cursors.get(&slot).ok_or(Error::InvalidCursor)?;
                if !state.binding.matches(subject, query, shape) {
                    return Err(Error::InvalidCursor);
                }
                (state.start, Some(Arc::clone(&state.binding)))
            }
            None => (0, None),
        };
        let (mut matches, examined, index_lookups, indexed) =
            self.selection_ordinals(subject, query)?;
        // Every source enumerates unique RowKeys in ascending order. Identity ordering
        // therefore needs no comparison sort; field ordering retains its stable tie-break.
        if query.order.field.is_some() {
            matches.sort_by(|a, b| self.compare_ordinals(*a, *b, &query.order));
        } else if query.order.descending {
            matches.reverse();
        }
        if start > matches.len() {
            return Err(Error::InvalidCursor);
        }
        let end = start.saturating_add(query.limit).min(matches.len());
        let next = if end < matches.len() {
            let binding = match binding {
                Some(binding) => binding,
                None => {
                    let mut request = profile_memory!(QueryCursors, query.clone());
                    request.cursor = None;
                    profile_memory!(
                        QueryCursors,
                        Arc::new(Binding {
                            subject: subject.clone(),
                            query: request,
                            shape: shape.owned()
                        })
                    )
                }
            };
            Some(self.issue(binding, end)?)
        } else {
            None
        };
        let plan = QueryPlan {
            strategy: if indexed {
                PlanStrategy::Indexed
            } else {
                PlanStrategy::Scan
            },
            index_lookups,
            candidates: examined,
            rows_examined: examined,
            snapshot_sequence: self.sequence,
        };
        #[cfg(feature = "memory-profile")]
        memory::publish_cursors(self);
        Ok(Selection {
            keys: matches,
            start,
            end,
            next,
            plan,
        })
    }
    fn selection_ordinals(
        &self,
        subject: &Subject,
        query: &Query,
    ) -> Result<(Vec<usize>, usize, u64, bool)> {
        let (selected, mut probes) = self.ordinal_candidates(&query.filter)?;
        let indexed = selected.is_some() || !query.schemas.is_empty();
        probes += query.schemas.len() as u64;
        let selected = if let Some(selected) = selected {
            selected
        } else {
            let mut keys = if !query.schemas.is_empty() {
                query
                    .schemas
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter_map(|schema| self.by_schema.get(schema))
                    .flat_map(|keys| keys.iter().copied())
                    .collect::<Vec<_>>()
            } else {
                query
                    .namespaces
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .filter_map(|ns| self.by_namespace.get(ns))
                    .flat_map(|keys| keys.iter().copied())
                    .collect::<Vec<_>>()
            };
            keys.sort_unstable();
            keys.dedup();
            OrdinalCandidates::Owned(keys)
        };
        let mut examined = 0;
        let mut visible = |ordinal: &usize| {
            let entry = &self.rows[*ordinal];
            let allowed = query.namespaces.contains(&entry.key.0)
                && entry
                    .row
                    .required_namespaces
                    .iter()
                    .all(|ns| subject.readable_namespaces.contains(ns))
                && (query.schemas.is_empty() || query.schemas.contains(&entry.row.schema));
            if !allowed {
                return false;
            }
            examined += 1;
            self.matches(&self.rows[*ordinal].row, &query.filter)
        };
        let keys = match selected {
            OrdinalCandidates::Borrowed(keys) => {
                keys.iter().copied().filter(&mut visible).collect()
            }
            OrdinalCandidates::Owned(keys) => keys.into_iter().filter(&mut visible).collect(),
        };
        Ok((keys, examined, probes, indexed))
    }
    #[cfg(test)]
    fn selection_keys(
        &self,
        subject: &Subject,
        query: &Query,
    ) -> Result<(Vec<RowKey>, usize, u64, bool)> {
        let (keys, n, p, i) = self.selection_ordinals(subject, query)?;
        Ok((
            keys.into_iter().map(|o| self.rows[o].key).collect(),
            n,
            p,
            i,
        ))
    }
    #[cfg(test)]
    fn legacy_selection_keys(
        &self,
        subject: &Subject,
        query: &Query,
    ) -> Result<(Vec<RowKey>, usize, u64, bool)> {
        self.legacy.selection_keys(self, subject, query)
    }
    fn validate_projection(
        &self,
        subject: &Subject,
        query: &Query,
        projection: &Projection,
    ) -> Result<()> {
        if projection.len() > 256 {
            return Err(Error::TooLarge);
        }
        if projection.is_empty() {
            return Err(invalid("projection must select fields"));
        }
        for (schema, positions) in projection {
            if positions.len() > 256 {
                return Err(Error::TooLarge);
            }
            if positions.is_empty() {
                return Err(invalid("projection selection must not be empty"));
            }
            let definition = self.decoder.schema(*schema)?;
            let mut seen = [false; 256];
            for position in positions {
                if *position >= definition.fields.len() {
                    return Err(invalid("projection field position out of bounds"));
                }
                if seen[*position] {
                    return Err(invalid("duplicate projection field position"));
                }
                seen[*position] = true;
            }
        }
        if !query.schemas.is_empty() {
            if query
                .schemas
                .iter()
                .any(|schema| !projection.contains_key(schema))
            {
                return Err(invalid("projection does not cover queried schemas"));
            }
        } else {
            for (schema, keys) in &self.by_schema {
                if projection.contains_key(schema) {
                    continue;
                }
                let visible = query.namespaces.iter().any(|ns| {
                    let lo = keys.partition_point(|&o| self.rows[o].key.0 < *ns);
                    let hi = keys.partition_point(|&o| self.rows[o].key.0 <= *ns);
                    keys[lo..hi].iter().any(|&o| {
                        self.rows[o]
                            .row
                            .required_namespaces
                            .iter()
                            .all(|required| subject.readable_namespaces.contains(required))
                    })
                });
                if visible {
                    return Err(invalid("projection does not cover visible schemas"));
                }
            }
        }
        Ok(())
    }

    fn issue(&mut self, binding: Arc<Binding>, start: usize) -> Result<Cursor> {
        #[cfg(feature = "memory-profile")]
        let _memory_origin = canonical_memory::scope(canonical_memory::Component::QueryCursors);
        let slot = self.next_slot;
        self.next_slot = slot
            .checked_add(1)
            .ok_or(invalid("cursor slot exhausted"))?;
        if self.cursors.len() >= MAX_CURSORS {
            self.cursors.pop_first();
        }
        self.cursors.insert(slot, CursorState { binding, start });
        let mut token = [0; 24];
        token[..16].copy_from_slice(&self.owner);
        token[16..].copy_from_slice(&slot.to_le_bytes());
        Ok(Cursor(token))
    }
    fn field_kind(&self, field: FieldRef) -> Result<&FieldType> {
        let schema = self.decoder.schema(field.schema)?;
        let kind = &schema
            .fields
            .get(field.position)
            .ok_or(invalid("field position out of bounds"))?
            .kind;
        if !supported(kind) {
            return Err(invalid("field type is not a query scalar"));
        }
        Ok(kind)
    }
    fn validate(&self, predicate: &Predicate, depth: usize, budget: &mut Budget) -> Result<()> {
        budget.nodes += 1;
        if depth > MAX_QUERY_DEPTH || budget.nodes > MAX_QUERY_NODES {
            return Err(Error::TooLarge);
        }
        match predicate {
            Predicate::True => (),
            Predicate::Eq(field, value) => {
                budget.value(value)?;
                scalar(self.field_kind(*field)?, value)?;
            }
            Predicate::Range {
                field,
                lower,
                upper,
            } => {
                let kind = self.field_kind(*field)?;
                let lo = lower
                    .as_ref()
                    .map(|v| {
                        budget.value(v)?;
                        scalar(kind, v)
                    })
                    .transpose()?;
                let hi = upper
                    .as_ref()
                    .map(|v| {
                        budget.value(v)?;
                        scalar(kind, v)
                    })
                    .transpose()?;
                if lo.as_ref().zip(hi.as_ref()).is_some_and(|(lo, hi)| lo > hi) {
                    return Err(invalid("range lower exceeds upper"));
                }
            }
            Predicate::And(children) | Predicate::Or(children) => {
                if children.len() > MAX_QUERY_NODES.saturating_sub(budget.nodes) {
                    return Err(Error::TooLarge);
                }
                for child in children {
                    self.validate(child, depth + 1, budget)?;
                }
            }
        }
        Ok(())
    }
    fn posting_value(&self, ordinal: usize, field: FieldRef) -> &Scalar {
        self.rows[ordinal].row.values[field.position]
            .as_ref()
            .expect("indexed scalar")
    }
    fn equality_posting(&self, field: FieldRef, value: &Scalar) -> &[usize] {
        let Some(keys) = self.secondary.get(&field) else {
            return &[];
        };
        let lo = keys.partition_point(|&o| self.posting_value(o, field) < value);
        let hi = keys.partition_point(|&o| self.posting_value(o, field) <= value);
        &keys[lo..hi]
    }
    fn equality_slices<'a>(
        &'a self,
        predicate: &Predicate,
        sets: &mut Vec<&'a [usize]>,
        probes: &mut u64,
    ) -> Result<()> {
        match predicate {
            Predicate::Eq(field, value) => {
                let value = scalar(self.field_kind(*field)?, value)?;
                *probes += 1;
                sets.push(self.equality_posting(*field, &value));
            }
            Predicate::And(children) => {
                for child in children {
                    self.equality_slices(child, sets, probes)?;
                }
            }
            _ => (),
        };
        Ok(())
    }
    fn ordinal_candidates(
        &self,
        predicate: &Predicate,
    ) -> Result<(Option<OrdinalCandidates<'_>>, u64)> {
        match predicate {
            Predicate::True => Ok((None, 0)),
            Predicate::Eq(field, value) => {
                let value = scalar(self.field_kind(*field)?, value)?;
                Ok((
                    Some(OrdinalCandidates::Borrowed(
                        self.equality_posting(*field, &value),
                    )),
                    1,
                ))
            }
            Predicate::Range {
                field,
                lower,
                upper,
            } => {
                let kind = self.field_kind(*field)?;
                let lo = lower.as_ref().map(|v| scalar(kind, v)).transpose()?;
                let hi = upper.as_ref().map(|v| scalar(kind, v)).transpose()?;
                let keys = self.secondary.get(field).map_or(&[][..], |v| v.as_ref());
                let start = lo.as_ref().map_or(0, |lo| {
                    keys.partition_point(|&o| self.posting_value(o, *field) < lo)
                });
                let end = hi.as_ref().map_or(keys.len(), |hi| {
                    keys.partition_point(|&o| self.posting_value(o, *field) <= hi)
                });
                let mut ordinals = keys[start..end].to_vec();
                ordinals.sort_unstable();
                ordinals.dedup();
                Ok((Some(OrdinalCandidates::Owned(ordinals)), 1))
            }
            Predicate::And(children) => {
                let mut sets = Vec::new();
                let mut probes = 0;
                self.equality_slices(predicate, &mut sets, &mut probes)?;
                if !sets.is_empty() {
                    sets.sort_by_key(|keys| keys.len());
                    if sets[0].is_empty() {
                        return Ok((Some(OrdinalCandidates::Borrowed(&[])), probes));
                    }
                    if sets.len() == 1 {
                        return Ok((Some(OrdinalCandidates::Borrowed(sets[0])), probes));
                    }
                    let mut keys = Vec::with_capacity(sets[0].len());
                    keys.extend(intersect_slices(sets[0], sets[1]).copied());
                    for set in sets.into_iter().skip(2) {
                        keys.retain(|key| set.binary_search(key).is_ok());
                    }
                    return Ok((Some(OrdinalCandidates::Owned(keys)), probes));
                }
                for child in children {
                    let (keys, n) = self.ordinal_candidates(child)?;
                    probes += n;
                    if keys.is_some() {
                        return Ok((keys, probes));
                    }
                }
                Ok((None, probes))
            }
            Predicate::Or(children) => {
                let mut keys = Vec::new();
                let mut probes = 0;
                let mut all = false;
                for child in children {
                    let (candidate, n) = self.ordinal_candidates(child)?;
                    probes += n;
                    if let Some(candidate) = candidate {
                        keys.extend_from_slice(candidate.as_slice());
                        keys.sort_unstable();
                        keys.dedup();
                    } else {
                        all = true;
                    }
                }
                Ok((
                    if all {
                        None
                    } else {
                        Some(OrdinalCandidates::Owned(keys))
                    },
                    probes,
                ))
            }
        }
    }
    fn value<'a>(&self, row: &'a Row, field: FieldRef) -> Option<&'a Scalar> {
        if row.schema != field.schema {
            return None;
        }
        row.values.get(field.position).and_then(Option::as_ref)
    }
    fn matches(&self, row: &Row, predicate: &Predicate) -> bool {
        match predicate {
            Predicate::True => true,
            Predicate::Eq(field, value) => self.value(row, *field).is_some_and(|actual| {
                scalar(self.field_kind(*field).expect("validated field"), value)
                    .is_ok_and(|expected| actual == &expected)
            }),
            Predicate::Range {
                field,
                lower,
                upper,
            } => self.value(row, *field).is_some_and(|actual| {
                let kind = self.field_kind(*field).expect("validated field");
                let lower = lower
                    .as_ref()
                    .map(|v| scalar(kind, v).expect("validated scalar"));
                let upper = upper
                    .as_ref()
                    .map(|v| scalar(kind, v).expect("validated scalar"));
                lower.as_ref().is_none_or(|lo| actual >= lo)
                    && upper.as_ref().is_none_or(|hi| actual <= hi)
            }),
            Predicate::And(children) => children.iter().all(|child| self.matches(row, child)),
            Predicate::Or(children) => children.iter().any(|child| self.matches(row, child)),
        }
    }
    fn compare_ordinals(&self, a: usize, b: usize, order: &Order) -> Ordering {
        if order.field.is_none() {
            return if order.descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            };
        }
        let first = order
            .field
            .map(|field| {
                self.value(&self.rows[a].row, field)
                    .cmp(&self.value(&self.rows[b].row, field))
            })
            .unwrap_or(Ordering::Equal);
        let first = if order.descending {
            first.reverse()
        } else {
            first
        };
        first.then_with(|| a.cmp(&b))
    }
    #[cfg(test)]
    fn compare(&self, a: RowKey, b: RowKey, order: &Order) -> Ordering {
        self.legacy.compare(self, a, b, order)
    }
}

enum OrdinalCandidates<'a> {
    Borrowed(&'a [usize]),
    Owned(Vec<usize>),
}
impl OrdinalCandidates<'_> {
    fn as_slice(&self) -> &[usize] {
        match self {
            Self::Borrowed(keys) => keys,
            Self::Owned(keys) => keys,
        }
    }
}
struct SliceIntersection<'a, T> {
    a: &'a [T],
    b: &'a [T],
    i: usize,
    j: usize,
    search: bool,
}
fn intersect_slices<'a, T: Ord>(a: &'a [T], b: &'a [T]) -> SliceIntersection<'a, T> {
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let empty = a.is_empty() || b.is_empty() || a.first() > b.last() || a.last() < b.first();
    SliceIntersection {
        a,
        b,
        i: if empty { a.len() } else { 0 },
        j: 0,
        search: a.len() <= b.len() / 16,
    }
}
impl<'a, T: Ord> Iterator for SliceIntersection<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.search {
            while let Some(key) = self.a.get(self.i) {
                self.i += 1;
                if self.b.binary_search(key).is_ok() {
                    return Some(key);
                }
            }
            return None;
        }
        while self.i < self.a.len() && self.j < self.b.len() {
            match self.a[self.i].cmp(&self.b[self.j]) {
                Ordering::Less => self.i += 1,
                Ordering::Greater => self.j += 1,
                Ordering::Equal => {
                    let key = &self.a[self.i];
                    self.i += 1;
                    self.j += 1;
                    return Some(key);
                }
            }
        }
        None
    }
}

#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
}
impl Budget {
    fn value(&mut self, value: &Value) -> Result<()> {
        let bytes = match value {
            Value::Utf8(v) => v.len(),
            Value::Bytes(v) => v.len(),
            _ => 32,
        };
        self.bytes = self.bytes.checked_add(bytes).ok_or(Error::TooLarge)?;
        if self.bytes > MAX_QUERY_LITERAL_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(())
    }
}
fn invalid(message: &'static str) -> Error {
    Error::InvalidInput(message)
}
fn session_owner<F>(fill: F) -> Result<[u8; 16]>
where
    F: FnOnce(&mut [u8]) -> std::result::Result<(), getrandom::Error>,
{
    let mut owner = [0; 16];
    fill(&mut owner).map_err(|_| invalid("query session entropy unavailable"))?;
    Ok(owner)
}
fn supported(kind: &FieldType) -> bool {
    !matches!(
        kind,
        FieldType::Array { .. } | FieldType::Struct(_) | FieldType::Relation
    )
}
fn scalar(kind: &FieldType, value: &Value) -> Result<Scalar> {
    Ok(match (kind, value) {
        (FieldType::Bool, Value::Bool(v)) => Scalar::Bool(*v),
        (FieldType::UInt, Value::UInt(v)) => Scalar::UInt(*v),
        (FieldType::Int, Value::Int(v)) => Scalar::Int(*v),
        (FieldType::FixedInt, Value::FixedInt(v)) => Scalar::FixedInt(*v),
        (
            FieldType::Decimal { scale },
            Value::Decimal {
                mantissa,
                scale: actual,
            },
        ) if scale == actual => Scalar::Decimal(*mantissa, *scale),
        (FieldType::Float, Value::Float(v)) if v.is_finite() => {
            let bits = if *v == 0.0 {
                0.0f64.to_bits()
            } else {
                v.to_bits()
            };
            Scalar::Float(if bits >> 63 == 1 {
                !bits
            } else {
                bits ^ (1 << 63)
            })
        }
        (FieldType::Enum { variants }, Value::Enum(v)) if variants.contains(v) => Scalar::Enum(*v),
        (FieldType::Ref, Value::Ref(v)) if v.id != 0 => Scalar::Ref(v.namespace, v.id),
        (FieldType::Utf8, Value::Utf8(v)) => Scalar::Utf8(v.clone()),
        (FieldType::Bytes, Value::Bytes(v)) => Scalar::Bytes(v.clone()),
        _ => return Err(invalid("predicate value does not match field type")),
    })
}

#[cfg(test)]
mod tests;
