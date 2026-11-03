//! Authorized bounded navigation over canonical native relations in one immutable snapshot.
use canonical_core::{Error, Result};
use canonical_query::Subject;
use canonical_registry::{
    NativeEdge, NewRecord, Record, Reference, Registry, RelationRule, Revision, SchemaRef,
    Snapshot, Value,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[cfg(feature = "memory-profile")]
mod memory;

pub const MAX_NEIGHBORS: usize = 1000;
pub const MAX_WALK_DEPTH: usize = 16;
pub const MAX_WALK_NODES: usize = 10_000;
pub const MAX_WALK_EDGES: usize = 10_000;

#[derive(Clone, Debug)]
pub struct WriteAccess {
    pub subject: Subject,
    pub writable_namespaces: BTreeSet<u64>,
}

/// Link or replace a native edge. The local integrator supplies trusted grants.
/// Edge namespace must be writable and both endpoint namespaces must be readable.
pub fn link(
    registry: &mut Registry,
    access: &WriteAccess,
    edge: NewRecord,
    expected_revision: u64,
) -> Result<Record> {
    authorize_write(
        access,
        Reference {
            namespace: edge.namespace,
            id: edge.id,
        },
    )?;
    if let Some(previous) = registry.native_edge(Reference {
        namespace: edge.namespace,
        id: edge.id,
    }) {
        authorize_edge(&access.subject, previous)?;
    }
    let rule = registry
        .relation_rule(edge.schema)
        .ok_or(invalid("schema is not bound to a native relation rule"))?;
    let (source, kind, target) = relation_value(&edge.values)?;
    if kind != rule.kind {
        return Err(invalid("native relation kind does not match rule"));
    }
    authorize_reference(&access.subject, source)?;
    authorize_reference(&access.subject, target)?;
    if expected_revision == 0 {
        registry.create(edge.namespace, edge.id, edge.schema, &edge.values)
    } else {
        registry.update(edge, expected_revision)
    }
}

pub fn unlink(
    registry: &mut Registry,
    access: &WriteAccess,
    edge: Reference,
    expected_revision: u64,
) -> Result<Revision> {
    authorize_write(access, edge)?;
    let previous = registry.native_edge(edge).ok_or(Error::NotFound)?;
    authorize_edge(&access.subject, previous)?;
    registry.delete(edge.namespace, edge.id, expected_revision)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Outgoing,
    Incoming,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighborhood {
    pub edges: Vec<NativeEdge>,
    pub has_more: bool,
}

#[derive(Clone, Debug)]
pub struct WalkOptions {
    pub direction: Direction,
    pub relation_schema: Option<SchemaRef>,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub max_edges: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walk {
    pub nodes: Vec<Reference>,
    pub edges: Vec<NativeEdge>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GraphLayout {
    #[default]
    Legacy,
    Compact,
}

/// Bidirectional adjacency is derived from a validated canonical snapshot, never authoritative.
struct CompactGraph {
    nodes: Vec<(Reference, SchemaRef)>,
    edges: Vec<NativeEdge>,
    sources: Csr,
    targets: Csr,
}
#[derive(Default)]
struct Csr {
    offsets: Vec<usize>,
    postings: Vec<usize>,
    #[cfg(feature = "memory-profile")]
    scratch_peak_bytes: usize,
}
impl Csr {
    fn build(
        nodes: &[(Reference, SchemaRef)],
        edges: &[NativeEdge],
        incoming: bool,
    ) -> Result<Self> {
        let n = nodes.len().checked_add(1).ok_or(Error::TooLarge)?;
        check_capacity::<usize>(n)?;
        check_capacity::<usize>(edges.len())?;
        let mut offsets = vec![0usize; n];
        for edge in edges {
            let endpoint = if incoming { edge.target } else { edge.source };
            let i = nodes
                .binary_search_by_key(&endpoint, |(r, _)| *r)
                .map_err(|_| invalid("missing endpoint"))?;
            offsets[i + 1] = offsets[i + 1].checked_add(1).ok_or(Error::TooLarge)?;
        }
        for i in 1..n {
            offsets[i] = offsets[i]
                .checked_add(offsets[i - 1])
                .ok_or(Error::TooLarge)?;
        }
        let mut postings = vec![0; edges.len()];
        let mut cursor = offsets[..nodes.len()].to_vec();
        for (ordinal, edge) in edges.iter().enumerate() {
            let endpoint = if incoming { edge.target } else { edge.source };
            let i = nodes
                .binary_search_by_key(&endpoint, |(r, _)| *r)
                .map_err(|_| invalid("missing endpoint"))?;
            postings[cursor[i]] = ordinal;
            cursor[i] = cursor[i].checked_add(1).ok_or(Error::TooLarge)?;
        }
        #[cfg(feature = "memory-profile")]
        let scratch_peak_bytes = cursor.capacity() * std::mem::size_of::<usize>();
        Ok(Self {
            offsets,
            postings,
            #[cfg(feature = "memory-profile")]
            scratch_peak_bytes,
        })
    }
}
/// Immutable, validated native-relation projection. `new` preserves the Legacy layout.
pub struct GraphView {
    compact: Option<CompactGraph>,
    nodes: BTreeMap<Reference, SchemaRef>,
    rules: BTreeMap<SchemaRef, RelationRule>,
    edges: BTreeMap<Reference, NativeEdge>,
    sources: BTreeMap<Reference, Vec<Reference>>,
    targets: BTreeMap<Reference, Vec<Reference>>,
}

impl GraphView {
    pub fn new(registry: &Registry, snapshot: Snapshot) -> Result<Self> {
        Self::new_with_layout(registry, snapshot, GraphLayout::Legacy)
    }
    pub fn layout(&self) -> GraphLayout {
        if self.compact.is_some() {
            GraphLayout::Compact
        } else {
            GraphLayout::Legacy
        }
    }
    fn node_schema(&self, node: Reference) -> Option<&SchemaRef> {
        if let Some(c) = &self.compact {
            c.nodes
                .binary_search_by_key(&node, |(r, _)| *r)
                .ok()
                .map(|i| &c.nodes[i].1)
        } else {
            self.nodes.get(&node)
        }
    }
    /// Build an immutable RAM projection. Compact uses dense CSR offsets for every live identity.
    /// Mutations require rebuilding from a fresh snapshot; no persistent format is changed.
    pub fn new_with_layout(
        registry: &Registry,
        snapshot: Snapshot,
        layout: GraphLayout,
    ) -> Result<Self> {
        #[cfg(feature = "memory-profile")]
        let _origin = canonical_memory::scope(canonical_memory::Component::GraphEdges);
        let (mut compact, nodes) = {
            #[cfg(feature = "memory-profile")]
            let _c = canonical_memory::scope(canonical_memory::Component::GraphNodes);
            let records = snapshot.iter().map(|r| {
                (
                    Reference {
                        namespace: r.namespace,
                        id: r.id,
                    },
                    r.schema,
                )
            });
            if layout == GraphLayout::Compact {
                (
                    Some(CompactGraph {
                        nodes: records.collect(),
                        edges: Vec::new(),
                        sources: Csr::default(),
                        targets: Csr::default(),
                    }),
                    BTreeMap::new(),
                )
            } else {
                (None, records.collect())
            }
        };
        let rules: BTreeMap<SchemaRef, RelationRule> = {
            #[cfg(feature = "memory-profile")]
            let _c = canonical_memory::scope(canonical_memory::Component::GraphNodes);
            registry
                .relation_rules()
                .map(|rule| (rule.schema, rule.clone()))
                .collect()
        };
        if let Some(c) = &mut compact {
            let edge_count = snapshot
                .iter()
                .filter(|r| rules.contains_key(&r.schema))
                .count();
            check_capacity::<NativeEdge>(edge_count)?;
            c.edges.reserve_exact(edge_count);
        }
        let mut view = Self {
            compact,
            nodes,
            rules,
            edges: BTreeMap::new(),
            sources: BTreeMap::new(),
            targets: BTreeMap::new(),
        };
        for record in snapshot.iter() {
            let Some(rule) = view.rules.get(&record.schema) else {
                continue;
            };
            let values = registry.decode_values(record.schema, &record.payload)?;
            let (source, kind, target) = relation_value(&values)?;
            if kind != rule.kind
                || view.node_schema(source) != Some(&rule.source_schema)
                || view.node_schema(target) != Some(&rule.target_schema)
            {
                return Err(invalid(
                    "native relation snapshot violates endpoint contract",
                ));
            }
            let version = registry
                .revision(record.namespace, record.id, record.revision)
                .ok_or(invalid("snapshot revision is unavailable"))?;
            if version.deleted
                || version.record != *record
                || version.commit_sequence > snapshot.commit_sequence
            {
                return Err(invalid(
                    "snapshot revision does not match canonical history",
                ));
            }
            let reference = Reference {
                namespace: record.namespace,
                id: record.id,
            };
            let edge = NativeEdge {
                reference,
                revision: record.revision,
                schema: record.schema,
                kind,
                source,
                target,
                commit_sequence: version.commit_sequence,
            };
            if let Some(c) = &mut view.compact {
                c.edges.push(edge);
            } else {
                view.edges.insert(reference, edge);
            }
        }
        // BTreeMap iteration ensures each adjacency list is sorted by the edge's reference.
        {
            #[cfg(feature = "memory-profile")]
            let _c = canonical_memory::scope(canonical_memory::Component::GraphAdjacency);
            if let Some(c) = &mut view.compact {
                c.edges.sort_unstable_by_key(|e| e.reference);
                {
                    #[cfg(feature = "memory-profile")]
                    let _c = canonical_memory::scope(canonical_memory::Component::GraphEdges);
                    c.edges.shrink_to_fit();
                }
                {
                    #[cfg(feature = "memory-profile")]
                    let _c = canonical_memory::scope(canonical_memory::Component::GraphNodes);
                    c.nodes.sort_unstable_by_key(|(r, _)| *r);
                    c.nodes.shrink_to_fit();
                }
                c.sources = Csr::build(&c.nodes, &c.edges, false)?;
                c.targets = Csr::build(&c.nodes, &c.edges, true)?;
            }
            for (reference, edge) in &view.edges {
                view.sources
                    .entry(edge.source)
                    .or_default()
                    .push(*reference);
                view.targets
                    .entry(edge.target)
                    .or_default()
                    .push(*reference);
            }
        }
        #[cfg(feature = "memory-profile")]
        memory::publish_graph(&view);
        Ok(view)
    }

    pub fn outgoing(
        &self,
        subject: &Subject,
        node: Reference,
        relation_schema: Option<SchemaRef>,
        limit: usize,
    ) -> Result<Neighborhood> {
        self.neighborhood(subject, node, relation_schema, limit, Direction::Outgoing)
    }
    pub fn incoming(
        &self,
        subject: &Subject,
        node: Reference,
        relation_schema: Option<SchemaRef>,
        limit: usize,
    ) -> Result<Neighborhood> {
        self.neighborhood(subject, node, relation_schema, limit, Direction::Incoming)
    }
    fn neighborhood(
        &self,
        subject: &Subject,
        node: Reference,
        relation_schema: Option<SchemaRef>,
        limit: usize,
        direction: Direction,
    ) -> Result<Neighborhood> {
        self.authorize_node(subject, node)?;
        self.validate_filter(relation_schema)?;
        if limit == 0 || limit > MAX_NEIGHBORS {
            return Err(invalid("neighbor limit must be 1..1000"));
        }
        let mut visible = self.adjacent(subject, node, direction, relation_schema);
        let edges = visible.by_ref().take(limit).cloned().collect();
        let has_more = visible.next().is_some();
        Ok(Neighborhood { edges, has_more })
    }

    pub fn walk(&self, subject: &Subject, start: Reference, options: &WalkOptions) -> Result<Walk> {
        self.authorize_node(subject, start)?;
        self.validate_filter(options.relation_schema)?;
        if options.max_depth == 0
            || options.max_depth > MAX_WALK_DEPTH
            || options.max_nodes == 0
            || options.max_nodes > MAX_WALK_NODES
            || options.max_edges == 0
            || options.max_edges > MAX_WALK_EDGES
        {
            return Err(invalid("walk limits exceeded"));
        }
        let mut result = Walk {
            nodes: vec![start],
            edges: Vec::new(),
            truncated: false,
        };
        let mut visited = BTreeSet::from([start]);
        let mut emitted = BTreeSet::new();
        let mut queue = VecDeque::from([(start, 0)]);
        while let Some((node, depth)) = queue.pop_front() {
            let mut adjacent =
                self.adjacent(subject, node, options.direction, options.relation_schema);
            if depth == options.max_depth {
                if adjacent.any(|edge| !emitted.contains(&edge.reference)) {
                    result.truncated = true;
                }
                continue;
            }
            for edge in adjacent {
                if emitted.contains(&edge.reference) {
                    continue;
                }
                let peer = match options.direction {
                    Direction::Outgoing => edge.target,
                    Direction::Incoming => edge.source,
                };
                if result.edges.len() == options.max_edges
                    || (!visited.contains(&peer) && result.nodes.len() == options.max_nodes)
                {
                    result.truncated = true;
                    return Ok(result);
                }
                emitted.insert(edge.reference);
                result.edges.push(edge.clone());
                if visited.insert(peer) {
                    result.nodes.push(peer);
                    queue.push_back((peer, depth + 1));
                }
            }
        }
        Ok(result)
    }

    fn authorize_node(&self, subject: &Subject, node: Reference) -> Result<()> {
        authorize_reference(subject, node)?;
        if self.node_schema(node).is_none() {
            return Err(Error::NotFound);
        }
        Ok(())
    }
    fn validate_filter(&self, relation_schema: Option<SchemaRef>) -> Result<()> {
        if relation_schema.is_some_and(|schema| !self.rules.contains_key(&schema)) {
            return Err(invalid("unknown native relation schema"));
        }
        Ok(())
    }
    fn adjacent<'a>(
        &'a self,
        subject: &'a Subject,
        node: Reference,
        direction: Direction,
        relation_schema: Option<SchemaRef>,
    ) -> impl Iterator<Item = &'a NativeEdge> {
        let adjacency = match direction {
            Direction::Outgoing => &self.sources,
            Direction::Incoming => &self.targets,
        };
        let compact = self.compact.as_ref().and_then(|c| {
            let i = c.nodes.binary_search_by_key(&node, |(r, _)| *r).ok()?;
            let csr = match direction {
                Direction::Outgoing => &c.sources,
                Direction::Incoming => &c.targets,
            };
            Some(
                csr.postings[csr.offsets[i]..csr.offsets[i + 1]]
                    .iter()
                    .map(move |&ordinal| &c.edges[ordinal]),
            )
        });
        adjacency
            .get(&node)
            .into_iter()
            .flatten()
            .filter_map(|reference| self.edges.get(reference))
            .chain(compact.into_iter().flatten())
            .filter(move |edge| {
                relation_schema.is_none_or(|schema| edge.schema == schema)
                    && edge_visible(subject, edge)
            })
    }
}

fn check_capacity<T>(count: usize) -> Result<()> {
    let bytes = count
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(Error::TooLarge)?;
    if bytes > isize::MAX as usize {
        return Err(Error::TooLarge);
    }
    Ok(())
}
fn invalid(message: &'static str) -> Error {
    Error::InvalidInput(message)
}
fn validate_subject(subject: &Subject) -> Result<()> {
    if subject.id.is_empty() {
        return Err(Error::Unauthorized);
    }
    if subject.id.len() > 1024 || subject.readable_namespaces.len() > 4096 {
        return Err(Error::TooLarge);
    }
    Ok(())
}
fn authorize_reference(subject: &Subject, reference: Reference) -> Result<()> {
    validate_subject(subject)?;
    if !subject.readable_namespaces.contains(&reference.namespace) {
        return Err(Error::Unauthorized);
    }
    if reference.id == 0 {
        return Err(invalid("zero record reference"));
    }
    Ok(())
}
fn authorize_write(access: &WriteAccess, edge: Reference) -> Result<()> {
    authorize_reference(&access.subject, edge)?;
    if !access.writable_namespaces.contains(&edge.namespace) {
        return Err(Error::Unauthorized);
    }
    if access.writable_namespaces.len() > 4096 {
        return Err(Error::TooLarge);
    }
    Ok(())
}
fn edge_visible(subject: &Subject, edge: &NativeEdge) -> bool {
    [
        edge.reference.namespace,
        edge.source.namespace,
        edge.target.namespace,
    ]
    .iter()
    .all(|namespace| subject.readable_namespaces.contains(namespace))
}
fn authorize_edge(subject: &Subject, edge: &NativeEdge) -> Result<()> {
    validate_subject(subject)?;
    if !edge_visible(subject, edge) {
        return Err(Error::Unauthorized);
    }
    Ok(())
}
fn relation_value(values: &[Option<Value>]) -> Result<(Reference, u64, Reference)> {
    match values {
        [
            Some(Value::Relation {
                source,
                kind,
                target,
            }),
        ] => Ok((*source, *kind, *target)),
        _ => Err(invalid(
            "native edge needs exactly one required relation value",
        )),
    }
}

#[cfg(test)]
mod tests;
