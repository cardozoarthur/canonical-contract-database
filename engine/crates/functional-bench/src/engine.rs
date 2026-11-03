//! Workload adapter using native APIs, not oracle-generated query answers.
use crate::common::{
    Backend, Config, Edge, Fact, Identity, ProjectedFields, ProjectionResult, ProjectionShape,
    Result,
};
use canonical_core::Error;
use canonical_query::{FieldRef, Order, PlanStrategy, Predicate, Query, QueryEngine, Subject};
use canonical_registry::{
    Field, FieldType, IndexPolicy, Level, Mutation, Namespace, NewRecord, Record, Reference,
    Registry, RelationRule, Schema, SchemaRef, Value,
};
use canonical_registry::{HistoryReadMode, LookupRequest};
use canonical_relations::{Direction, GraphLayout, GraphView, WalkOptions};
use canonical_views::{Format, Limits, Reader};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
const EDGE_SCHEMA: SchemaRef = SchemaRef {
    namespace: 2,
    id: 1,
    version: 1,
};
fn sr(version: u32) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id: 1,
        version,
    }
}
fn schema(version: u32) -> Result<Schema> {
    if !(1..=2).contains(&version) {
        return Err("unsupported benchmark schema".into());
    }
    let mut fields = vec![
        Field {
            label: "action".into(),
            kind: FieldType::UInt,
            optional: false,
        },
        Field {
            label: "objective".into(),
            kind: FieldType::UInt,
            optional: false,
        },
        Field {
            label: "text".into(),
            kind: FieldType::Utf8,
            optional: false,
        },
    ];
    if version == 2 {
        fields.push(Field {
            label: "extra".into(),
            kind: FieldType::UInt,
            optional: true,
        });
    }
    Ok(Schema {
        reference: sr(version),
        fields,
    })
}
fn input(f: &Fact) -> NewRecord {
    let mut values = vec![
        Some(Value::UInt(f.action)),
        Some(Value::UInt(f.objective)),
        Some(Value::Utf8(f.text.clone())),
    ];
    if f.schema_version == 2 {
        values.push(f.extra.map(Value::UInt));
    }
    NewRecord {
        namespace: 1,
        id: f.id,
        schema: sr(f.schema_version),
        values,
    }
}
fn decoded(registry: &Registry, record: &Record) -> Result<Fact> {
    if record.namespace != 1
        || record.schema.namespace != 1
        || record.schema.id != 1
        || !(1..=2).contains(&record.schema.version)
    {
        return Err("unexpected fact schema".into());
    }
    let values = registry.decode_values(record.schema, &record.payload)?;
    let expected = if record.schema.version == 1 { 3 } else { 4 };
    if values.len() != expected {
        return Err("unexpected fact cardinality".into());
    }
    let action = match values[0] {
        Some(Value::UInt(v)) => v,
        _ => return Err("invalid action".into()),
    };
    let objective = match values[1] {
        Some(Value::UInt(v)) => v,
        _ => return Err("invalid objective".into()),
    };
    let text = match &values[2] {
        Some(Value::Utf8(v)) => v.clone(),
        _ => return Err("invalid text".into()),
    };
    let extra = if expected == 4 {
        match values[3] {
            Some(Value::UInt(v)) => Some(v),
            None => None,
            _ => return Err("invalid extra".into()),
        }
    } else {
        None
    };
    Ok(Fact {
        id: record.id,
        revision: record.revision,
        schema_version: record.schema.version,
        action,
        objective,
        text,
        extra,
    })
}
fn subject() -> Subject {
    Subject {
        id: "functional-benchmark-authorized".into(),
        readable_namespaces: BTreeSet::from([1, 2]),
    }
}
#[derive(Clone, Copy, Debug)]
pub struct EngineOptions {
    pub index_policy: IndexPolicy,
    pub graph_layout: GraphLayout,
    pub history_mode: HistoryReadMode,
}
impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            index_policy: IndexPolicy::Full,
            graph_layout: GraphLayout::Legacy,
            history_mode: HistoryReadMode::Selective,
        }
    }
}
struct Engine {
    registry: Registry,
    options: EngineOptions,
    cfg: Config,
    next_transaction: u64,
    query: Option<QueryEngine>,
    graph: Option<GraphView>,
    filter_pages: u64,
    indexed_pages: u64,
    filter_index_lookups: u64,
    filter_candidates: u64,
    filter_rows_examined: u64,
}
pub fn open(path: &Path, new: bool, cfg: Config) -> Result<Box<dyn Backend>> {
    open_with_policy(path, new, cfg, IndexPolicy::Full)
}
pub fn open_with_policy(
    path: &Path,
    new: bool,
    cfg: Config,
    policy: IndexPolicy,
) -> Result<Box<dyn Backend>> {
    open_with_options(
        path,
        new,
        cfg,
        EngineOptions {
            index_policy: policy,
            ..EngineOptions::default()
        },
    )
}
pub fn open_with_options(
    path: &Path,
    new: bool,
    cfg: Config,
    options: EngineOptions,
) -> Result<Box<dyn Backend>> {
    cfg.validate()?;
    if new && path.exists() && std::fs::read_dir(path)?.next().is_some() {
        return Err("new directory is not empty".into());
    }
    if !new && !path.join("manifest.cbr").is_file() {
        return Err("reopen requires existing database".into());
    }
    let registry = Registry::open_with_index_policy(path, options.index_policy)?;
    let next_transaction = registry
        .snapshot()
        .commit_sequence
        .checked_add(1)
        .ok_or("transaction sequence exhausted")?;
    Ok(Box::new(Engine {
        registry,
        options,
        cfg,
        next_transaction,
        query: None,
        graph: None,
        filter_pages: 0,
        indexed_pages: 0,
        filter_index_lookups: 0,
        filter_candidates: 0,
        filter_rows_examined: 0,
    }))
}
impl Engine {
    fn filter_request(&self, action: u64, objective: u64) -> Query {
        let schemas = self
            .registry
            .schemas()
            .filter(|s| s.reference.namespace == 1 && s.reference.id == 1)
            .map(|s| s.reference)
            .collect::<Vec<_>>();
        let filter = Predicate::Or(
            schemas
                .iter()
                .map(|s| {
                    Predicate::And(vec![
                        Predicate::Eq(
                            FieldRef {
                                schema: *s,
                                position: 0,
                            },
                            Value::UInt(action),
                        ),
                        Predicate::Eq(
                            FieldRef {
                                schema: *s,
                                position: 1,
                            },
                            Value::UInt(objective),
                        ),
                    ])
                })
                .collect(),
        );
        Query {
            namespaces: vec![1],
            schemas,
            filter,
            order: Order::default(),
            limit: 1000,
            cursor: None,
        }
    }
    fn transaction(&mut self) -> Result<[u8; 16]> {
        let next = self.next_transaction;
        self.next_transaction = next
            .checked_add(1)
            .ok_or("transaction sequence exhausted")?;
        let mut id = [0; 16];
        id[..8].copy_from_slice(&next.to_le_bytes());
        id[8..].copy_from_slice(b"E11BENCH");
        Ok(id)
    }
    fn invalidate(&mut self) {
        self.query = None;
        self.graph = None;
    }
}
impl Backend for Engine {
    fn register_schema(&mut self, version: u32) -> Result<()> {
        if version == 1 {
            for (id, name) in [(1, "facts"), (2, "edges")] {
                self.registry.register_namespace(Namespace {
                    id,
                    name: name.into(),
                    level: Level::Local,
                })?;
            }
        }
        self.registry.register_schema(schema(version)?)?;
        if version == 1 {
            self.registry.register_schema(Schema {
                reference: EDGE_SCHEMA,
                fields: vec![Field {
                    label: "relation".into(),
                    kind: FieldType::Relation,
                    optional: false,
                }],
            })?;
            self.registry.register_relation_rule(RelationRule {
                schema: EDGE_SCHEMA,
                kind: 1,
                source_schema: sr(1),
                target_schema: sr(1),
            })?;
        }
        self.invalidate();
        Ok(())
    }
    fn write_facts(&mut self, batch: &[Fact]) -> Result<()> {
        let Some(first) = batch.first() else {
            return Err("empty fact batch".into());
        };
        if batch.len() > 256
            || first.revision == 0
            || batch.iter().any(|f| {
                f.revision != first.revision
                    || f.id == 0
                    || f.id > self.cfg.rows
                    || !(1..=2).contains(&f.schema_version)
            })
        {
            return Err("invalid fact batch".into());
        }
        let id = self.transaction()?;
        if first.revision == 1 {
            self.registry
                .create_batch(id, &batch.iter().map(input).collect::<Vec<_>>())?;
        } else {
            self.registry.commit(
                id,
                &batch
                    .iter()
                    .map(|f| Mutation::Update {
                        record: input(f),
                        expected_revision: f.revision - 1,
                    })
                    .collect::<Vec<_>>(),
            )?;
        }
        self.invalidate();
        Ok(())
    }
    fn write_edges(&mut self, batch: &[Edge]) -> Result<()> {
        if batch.is_empty()
            || batch.len() > 256
            || batch
                .iter()
                .any(|e| e.id == 0 || e.id >= self.cfg.graph_nodes())
        {
            return Err("invalid edge batch".into());
        }
        let records = batch
            .iter()
            .map(|e| NewRecord {
                namespace: 2,
                id: e.id,
                schema: EDGE_SCHEMA,
                values: vec![Some(Value::Relation {
                    source: Reference {
                        namespace: 1,
                        id: e.source,
                    },
                    kind: e.kind,
                    target: Reference {
                        namespace: 1,
                        id: e.target,
                    },
                })],
            })
            .collect::<Vec<_>>();
        let id = self.transaction()?;
        self.registry.create_batch(id, &records)?;
        self.invalidate();
        Ok(())
    }
    fn rejects_linked_schema_change(&mut self, cfg: Config) -> Result<bool> {
        let before = self
            .registry
            .record(1, 1)
            .ok_or("missing linked node")?
            .clone();
        let migrated = crate::common::fact(cfg, 1, cfg.revisions + 1);
        let rejected = matches!(
            self.registry.update(input(&migrated), cfg.revisions),
            Err(Error::Conflict)
        );
        Ok(rejected
            && self.registry.record(1, 1) == Some(&before)
            && self.registry.history(1, 1).len() == cfg.revisions as usize)
    }
    fn flush(&mut self) -> Result<()> {
        self.registry.flush_index()?;
        Ok(())
    }
    fn project(&mut self) -> Result<()> {
        let snapshot = self.registry.snapshot();
        self.query = Some(QueryEngine::new(&self.registry, snapshot.clone())?);
        self.graph = Some(GraphView::new_with_layout(
            &self.registry,
            snapshot,
            self.options.graph_layout,
        )?);
        Ok(())
    }
    fn filter_record_identities(&mut self, action: u64, objective: u64) -> Result<Vec<Identity>> {
        let mut spec = self.filter_request(action, objective);
        let query = self
            .query
            .as_mut()
            .ok_or("secondary projections not built")?;
        let mut identities = Vec::new();
        loop {
            // Legacy Record materialization: read metadata, then drop payload without decoding Values/Fact.
            let page = query.query(&subject(), &spec)?;
            identities.extend(page.records.into_iter().map(|r| Identity {
                id: r.id,
                revision: r.revision,
                schema_version: r.schema.version,
            }));
            match page.next {
                Some(cursor) => spec.cursor = Some(cursor),
                None => break,
            }
        }
        Ok(identities)
    }
    fn filter_facts(&mut self, action: u64, objective: u64) -> Result<Vec<Fact>> {
        let mut spec = self.filter_request(action, objective);
        let query = self
            .query
            .as_mut()
            .ok_or("secondary projections not built")?;
        let mut facts = Vec::new();
        loop {
            let page = query.query(&subject(), &spec)?;
            for record in page.records {
                facts.push(decoded(&self.registry, &record)?);
            }
            match page.next {
                Some(cursor) => spec.cursor = Some(cursor),
                None => break,
            }
        }
        Ok(facts)
    }
    fn select(
        &mut self,
        action: u64,
        objective: u64,
        shape: ProjectionShape,
    ) -> Result<ProjectionResult> {
        if shape == ProjectionShape::Full {
            return self
                .filter_facts(action, objective)
                .map(ProjectionResult::Full);
        }
        let mut spec = self.filter_request(action, objective);
        let projection = spec
            .schemas
            .iter()
            .map(|schema| {
                (
                    *schema,
                    if schema.version == 1 {
                        vec![0, 1]
                    } else {
                        vec![0, 1, 3]
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let query = self
            .query
            .as_mut()
            .ok_or("secondary projections not built")?;
        let mut ids = Vec::new();
        let mut rows = Vec::new();
        loop {
            let next = match shape {
                ProjectionShape::Ids => {
                    let page = query.query_ids(&subject(), &spec)?;
                    for id in page.ids {
                        if id.namespace != 1 || id.schema.namespace != 1 || id.schema.id != 1 {
                            return Err("IDs selection escaped workload namespace/schema".into());
                        }
                        ids.push(Identity {
                            id: id.id,
                            revision: id.revision,
                            schema_version: id.schema.version,
                        });
                    }
                    page.next
                }
                ProjectionShape::Fields => {
                    let page = query.query_projected(&subject(), &spec, &projection)?;
                    for row in page.rows {
                        let id = row.identity;
                        if id.namespace != 1 || id.schema.namespace != 1 || id.schema.id != 1 {
                            return Err("field selection escaped workload namespace/schema".into());
                        }
                        let expected = if id.schema.version == 1 { 2 } else { 3 };
                        if row.fields.len() != expected {
                            return Err("projected field count differs".into());
                        }
                        let number = |value: &Option<Value>| -> Result<u64> {
                            match value {
                                Some(Value::UInt(n)) => Ok(*n),
                                _ => Err("projected UInt missing/type differs".into()),
                            }
                        };
                        let action = number(&row.fields[0])?;
                        let objective = number(&row.fields[1])?;
                        let extra = if expected == 3 {
                            match &row.fields[2] {
                                Some(Value::UInt(n)) => Some(*n),
                                None => None,
                                _ => return Err("projected optional UInt type differs".into()),
                            }
                        } else {
                            None
                        };
                        rows.push(ProjectedFields {
                            identity: Identity {
                                id: id.id,
                                revision: id.revision,
                                schema_version: id.schema.version,
                            },
                            action,
                            objective,
                            extra,
                        });
                    }
                    page.next
                }
                ProjectionShape::Full => unreachable!(),
            };
            match next {
                Some(cursor) => spec.cursor = Some(cursor),
                None => break,
            }
        }
        Ok(match shape {
            ProjectionShape::Ids => ProjectionResult::Ids(ids),
            ProjectionShape::Fields => ProjectionResult::Fields(rows),
            ProjectionShape::Full => unreachable!(),
        })
    }
    fn filter(&mut self, action: u64, objective: u64) -> Result<Vec<u64>> {
        let mut spec = self.filter_request(action, objective);
        let query = self
            .query
            .as_mut()
            .ok_or("secondary projections not built")?;
        let mut ids = Vec::new();
        loop {
            let page = query.query(&subject(), &spec)?;
            self.filter_pages += 1;
            self.indexed_pages += u64::from(page.plan.strategy == PlanStrategy::Indexed);
            self.filter_index_lookups += page.plan.index_lookups;
            self.filter_candidates += page.plan.candidates as u64;
            self.filter_rows_examined += page.plan.rows_examined as u64;
            ids.extend(page.records.into_iter().map(|r| r.id));
            match page.next {
                Some(cursor) => spec.cursor = Some(cursor),
                None => break,
            }
        }
        Ok(ids)
    }
    fn walk(&mut self, start: u64, depth: usize, incoming: bool) -> Result<Vec<Edge>> {
        let graph = self
            .graph
            .as_ref()
            .ok_or("adjacency projection not built")?;
        let result = graph.walk(
            &subject(),
            Reference {
                namespace: 1,
                id: start,
            },
            &WalkOptions {
                direction: if incoming {
                    Direction::Incoming
                } else {
                    Direction::Outgoing
                },
                relation_schema: Some(EDGE_SCHEMA),
                max_depth: depth,
                max_nodes: 10000,
                max_edges: 10000,
            },
        )?;
        let mut edges = result
            .edges
            .into_iter()
            .map(|e| Edge {
                id: e.reference.id,
                source: e.source.id,
                kind: e.kind,
                target: e.target.id,
            })
            .collect::<Vec<_>>();
        edges.sort();
        Ok(edges)
    }
    fn history(&mut self, id: u64, first: u64, last: u64) -> Result<Vec<Fact>> {
        if first == 0 || last < first || last - first > 20 {
            return Err("invalid history window".into());
        }
        let requests = (first..=last)
            .map(|revision| LookupRequest {
                namespace: 1,
                id,
                revision: Some(revision),
            })
            .collect::<Vec<_>>();
        let batch = self
            .registry
            .lookup_many(&requests, self.options.history_mode)?;
        batch
            .results
            .into_iter()
            .map(|result| {
                let r = result.revision.ok_or("missing revision")?;
                decoded(&self.registry, &r.record)
            })
            .collect()
    }

    fn json(&mut self, id: u64) -> Result<Vec<u8>> {
        let record = self
            .registry
            .lookup_many(
                &[LookupRequest {
                    namespace: 1,
                    id,
                    revision: None,
                }],
                self.options.history_mode,
            )?
            .results
            .into_iter()
            .next()
            .ok_or("JSON lookup result missing")?
            .revision
            .ok_or("missing JSON head")?
            .record;
        let reader = Reader::new(
            &self.registry,
            Limits {
                max_records: 1,
                max_output_bytes: 1024 * 1024,
            },
        )?;
        let mut bytes = Vec::new();
        reader.write_one(Format::Json, Some(&record), &mut bytes)?;
        Ok(bytes)
    }
    fn scan(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        let snapshot = self.registry.snapshot();
        for record in snapshot.iter() {
            match record.namespace {
                1 if record.id > 0 && record.id <= self.cfg.rows => {
                    for revision in self.registry.history(1, record.id) {
                        if revision.deleted {
                            return Err("unexpected tombstone".into());
                        }
                        visitor(&decoded(&self.registry, &revision.record)?)?;
                    }
                }
                2 if record.id > 0 && record.id < self.cfg.graph_nodes() => (),
                _ => return Err("unexpected record outside workload scan".into()),
            }
        }
        Ok(())
    }
    fn scan_heads(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        let snapshot = self.registry.snapshot();
        for record in snapshot.iter() {
            match record.namespace {
                1 if record.id > 0 && record.id <= self.cfg.rows => {
                    visitor(&decoded(&self.registry, record)?)?
                }
                2 => (),
                _ => return Err("unexpected canonical current head outside workload".into()),
            }
        }
        Ok(())
    }
    fn scan_edges(&mut self, visitor: &mut dyn FnMut(Edge) -> Result<()>) -> Result<()> {
        let snapshot = self.registry.snapshot();
        for record in snapshot.iter().filter(|r| r.namespace != 1) {
            if record.namespace != 2
                || record.id == 0
                || record.id >= self.cfg.graph_nodes()
                || record.schema != EDGE_SCHEMA
                || record.revision != 1
            {
                return Err("unexpected canonical edge outside workload".into());
            }
            let edge = self
                .registry
                .native_edge(Reference {
                    namespace: record.namespace,
                    id: record.id,
                })
                .ok_or("unbound canonical edge")?;
            if edge.source.namespace != 1 || edge.target.namespace != 1 {
                return Err("unexpected endpoint namespace".into());
            }
            visitor(Edge {
                id: edge.reference.id,
                source: edge.source.id,
                kind: edge.kind,
                target: edge.target.id,
            })?;
        }
        Ok(())
    }
    fn details(&self) -> BTreeMap<String, serde_json::Value> {
        let facts = (1..=self.cfg.rows)
            .flat_map(|id| self.registry.history(1, id))
            .map(|v| v.record.payload.len() as u64)
            .sum::<u64>();
        let edges = (1..self.cfg.graph_nodes())
            .flat_map(|id| self.registry.history(2, id))
            .map(|v| v.record.payload.len() as u64)
            .sum::<u64>();
        BTreeMap::from([
            (
                "filter_plan_accumulated".into(),
                serde_json::json!({"pages":self.filter_pages,"indexed_pages":self.indexed_pages,"scan_pages":self.filter_pages-self.indexed_pages,"index_lookups":self.filter_index_lookups,"candidates_sum_per_page":self.filter_candidates,"rows_examined_sum_per_page":self.filter_rows_examined}),
            ),
            (
                "value_payload_bytes".into(),
                serde_json::json!(facts + edges),
            ),
            (
                "value_payload_definition".into(),
                serde_json::json!(
                    "typed positional values only; excludes identity/schema references/event and frame overhead/registry; compare complete disk totals"
                ),
            ),
            (
                "graph_layout".into(),
                serde_json::json!(format!("{:?}", self.options.graph_layout)),
            ),
            (
                "history_read_mode".into(),
                serde_json::json!(format!("{:?}", self.options.history_mode)),
            ),
            (
                "primary_cache_policy".into(),
                serde_json::json!(format!("{:?}", self.registry.index_policy())),
            ),
            ("primary_cache_status".into(), {
                let status = self.registry.index_status();
                serde_json::json!({"loaded":status.loaded,"rebuilt":status.rebuilt,"reason":status.reason.map(|reason|format!("{reason:?}"))})
            }),
            (
                "primary_index".into(),
                serde_json::json!(
                    "optional derived cache; authoritative replay validates exact cache equality by streaming; replay-only ignores existing cache and does not write it"
                ),
            ),
            (
                "secondary_and_graph_indices".into(),
                serde_json::json!(
                    "RAM projections; project includes QueryEngine and GraphView construction; Store and Registry retain distinct representations; Query shares its immutable Snapshot and rows have no owned payload, while scalar/index values remain materialized"
                ),
            ),
            (
                "history_access".into(),
                serde_json::json!(
                    "physical primary-index point read of containing canonical transaction frame per revision"
                ),
            ),
            (
                "json_access".into(),
                serde_json::json!(
                    "physical primary-index lookup plus canonical-views Reader::write_one on demand"
                ),
            ),
            (
                "point_reads".into(),
                serde_json::json!(self.registry.storage_stats().point_reads),
            ),
            (
                "replay_frames".into(),
                serde_json::json!(self.registry.storage_stats().replay_frames),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{self, edge, fact};
    fn directory() -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "canonical-functional-engine-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    fn config() -> Config {
        Config {
            rows: 40,
            revisions: 11,
            samples: 8,
        }
    }
    fn populated(path: &Path) -> Box<dyn Backend> {
        let cfg = config();
        let mut db = open(path, true, cfg).unwrap();
        db.register_schema(1).unwrap();
        for revision in 1..=cfg.revisions {
            db.write_facts(
                &(1..=cfg.rows)
                    .map(|id| fact(cfg, id, revision))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
        db.write_edges(&(1..cfg.graph_nodes()).map(edge).collect::<Vec<_>>())
            .unwrap();
        db.register_schema(2).unwrap();
        assert!(db.rejects_linked_schema_change(cfg).unwrap());
        db.write_facts(
            &((cfg.graph_nodes() + 1)..=cfg.rows)
                .map(|id| fact(cfg, id, cfg.revisions + 1))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        db.flush().unwrap();
        db
    }
    #[test]
    fn engine_history_graph_options_preserve_real_results() {
        let cfg = config();
        let path = directory();
        drop(populated(&path));
        for graph_layout in [GraphLayout::Legacy, GraphLayout::Compact] {
            for history_mode in [
                HistoryReadMode::ReferenceFull,
                HistoryReadMode::Selective,
                HistoryReadMode::Batch,
            ] {
                let mut db = open_with_options(
                    &path,
                    false,
                    cfg,
                    EngineOptions {
                        index_policy: IndexPolicy::Delta,
                        graph_layout,
                        history_mode,
                    },
                )
                .unwrap();
                db.project().unwrap();
                assert_eq!(
                    db.walk(1, 3, false).unwrap(),
                    common::walk(cfg, 1, 3, false)
                );
                assert_eq!(
                    db.walk(10, 3, true).unwrap(),
                    common::walk(cfg, 10, 3, true)
                );
                assert_eq!(
                    db.history(21, 2, 11).unwrap(),
                    (2..=11).map(|r| fact(cfg, 21, r)).collect::<Vec<_>>()
                );
                let details = db.details();
                assert_eq!(details["graph_layout"], format!("{graph_layout:?}"));
                assert_eq!(details["history_read_mode"], format!("{history_mode:?}"));
            }
        }
    }
    #[test]
    fn engine_index_policies_preserve_filter_results() {
        let cfg = config();
        let path = directory();
        drop(populated(&path));
        for policy in [
            IndexPolicy::Full,
            IndexPolicy::Grouped,
            IndexPolicy::Delta,
            IndexPolicy::ReplayOnly,
        ] {
            let mut db = open_with_policy(&path, false, cfg, policy).unwrap();
            db.project().unwrap();
            assert_eq!(db.filter(2, 2).unwrap(), common::matches(cfg, 2, 2));
            let details = db.details();
            assert_eq!(details["primary_cache_policy"], format!("{policy:?}"));
        }
    }
    #[test]
    fn engine_record_identity_baseline() {
        use crate::common::{ProjectionResult, ProjectionShape};
        let path = directory();
        let mut db = populated(&path);
        db.project().unwrap();
        for (action, objective) in [(2, 2), (1, 5), (999, 999)] {
            let baseline = db.filter_record_identities(action, objective).unwrap();
            assert_eq!(
                db.select(action, objective, ProjectionShape::Ids).unwrap(),
                ProjectionResult::Ids(baseline)
            );
        }
    }
    #[test]
    fn engine_projection_outputs() {
        use crate::common::{ProjectionShape, project_facts};
        let cfg = config();
        let path = directory();
        drop(populated(&path));
        let mut db = open(&path, false, cfg).unwrap();
        db.project().unwrap();
        for (action, objective) in [(2, 2), (1, 5), (999, 999)] {
            let expected = common::matches(cfg, action, objective)
                .into_iter()
                .map(|id| fact(cfg, id, cfg.latest(id)))
                .collect::<Vec<_>>();
            assert_eq!(db.filter_facts(action, objective).unwrap(), expected);
            for shape in [
                ProjectionShape::Ids,
                ProjectionShape::Fields,
                ProjectionShape::Full,
            ] {
                assert_eq!(
                    db.select(action, objective, shape).unwrap(),
                    project_facts(expected.clone(), shape)
                );
            }
        }
    }
    #[test]
    fn filters_graph_history_json_and_schema_migration_match_oracle_after_reopen() {
        let cfg = config();
        let path = directory();
        drop(populated(&path));
        let mut db = open(&path, false, cfg).unwrap();
        db.project().unwrap();
        for (action, objective) in [(1, 1), (2, 2), (6, 7), (7, 17)] {
            assert_eq!(
                db.filter(action, objective).unwrap(),
                common::matches(cfg, action, objective)
            );
        }
        for (start, incoming) in [(1, false), (10, false), (20, true), (10, true)] {
            assert_eq!(
                db.walk(start, 3, incoming).unwrap(),
                common::walk(cfg, start, 3, incoming)
            );
        }
        for id in [1, 20, 21, 40] {
            let last = cfg.latest(id) - 1;
            assert_eq!(
                db.history(id, last - 9, last).unwrap(),
                ((last - 9)..=last)
                    .map(|rev| fact(cfg, id, rev))
                    .collect::<Vec<_>>()
            );
            let actual: serde_json::Value = serde_json::from_slice(&db.json(id).unwrap()).unwrap();
            assert_eq!(
                actual,
                common::canonical_json(&fact(cfg, id, cfg.latest(id)))
            );
        }
    }
    #[test]
    fn scan_returns_every_canonical_revision_and_edge_without_duplicates() {
        let cfg = config();
        let path = directory();
        let mut db = populated(&path);
        let mut seen = std::collections::BTreeSet::new();
        db.scan(&mut |f| {
            assert_eq!(*f, fact(cfg, f.id, f.revision));
            assert!(seen.insert((f.id, f.revision)));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len() as u64, cfg.facts());
        let mut heads = std::collections::BTreeSet::new();
        db.scan_heads(&mut |f| {
            assert_eq!(*f, fact(cfg, f.id, cfg.latest(f.id)));
            assert!(heads.insert(f.id));
            Ok(())
        })
        .unwrap();
        assert_eq!(heads.len() as u64, cfg.rows);
        let mut edges = Vec::new();
        db.scan_edges(&mut |e| {
            edges.push(e);
            Ok(())
        })
        .unwrap();
        assert_eq!(edges, (1..cfg.graph_nodes()).map(edge).collect::<Vec<_>>());
    }
    #[test]
    fn dataset_bounds_and_new_reopen_modes_do_not_hide_unscanned_records() {
        let cfg = config();
        let path = directory();
        let mut db = open(&path, true, cfg).unwrap();
        db.register_schema(1).unwrap();
        let invalid = fact(cfg, cfg.rows + 1, 1);
        assert!(db.write_facts(&[invalid]).is_err());
        let mut seen = 0;
        db.scan(&mut |_| {
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 0);
        drop(db);
        assert!(open(&path, true, cfg).is_err());
        let missing = path.join("does-not-exist");
        assert!(open(&missing, false, cfg).is_err());
        assert!(!missing.exists());
    }
    #[test]
    fn scans_reject_extra_live_canonical_identities_instead_of_hiding_them() {
        let cfg = config();
        let path = directory();
        let registry = Registry::open(&path).unwrap();
        let mut db = Engine {
            registry,
            options: EngineOptions::default(),
            cfg,
            next_transaction: 1,
            query: None,
            graph: None,
            filter_pages: 0,
            indexed_pages: 0,
            filter_index_lookups: 0,
            filter_candidates: 0,
            filter_rows_examined: 0,
        };
        db.register_schema(1).unwrap();
        let extra = fact(cfg, cfg.rows + 1, 1);
        let r = input(&extra);
        db.registry.create(1, r.id, r.schema, &r.values).unwrap();
        assert!(db.scan(&mut |_| Ok(())).is_err());
        // A fresh partial workload lets the edge scanner reject an otherwise valid extra native edge.
        let path = directory();
        db.registry = Registry::open(&path).unwrap();
        db.register_schema(1).unwrap();
        db.write_facts(&[fact(cfg, 1, 1), fact(cfg, 2, 1)]).unwrap();
        db.registry
            .create(
                2,
                cfg.graph_nodes() + 1,
                EDGE_SCHEMA,
                &[Some(Value::Relation {
                    source: Reference {
                        namespace: 1,
                        id: 1,
                    },
                    kind: 1,
                    target: Reference {
                        namespace: 1,
                        id: 2,
                    },
                })],
            )
            .unwrap();
        assert!(db.scan_edges(&mut |_| Ok(())).is_err());
    }
}
