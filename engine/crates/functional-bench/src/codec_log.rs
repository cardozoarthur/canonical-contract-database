//! Codec wrappers share canonical-storage's physical frames/sync; all logical registry,
//! revision validation, key/location indices and graph projections below are harness code.
use crate::common::{
    self, Backend, Config, Edge, Fact, Identity, ProjectedFields, ProjectionResult,
    ProjectionShape, Result,
};
use canonical_storage::Store;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[derive(Clone, Copy)]
enum Codec {
    MessagePack,
    Protobuf,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Schema {
    version: u32,
    rows: u64,
    revisions: u64,
    fields: Vec<String>,
}
fn schema(cfg: Config, version: u32) -> Result<Schema> {
    let mut fields = vec![
        "id:u64",
        "revision:u64",
        "schema_version:u32",
        "action:u64",
        "objective:u64",
        "text:utf8",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    match version {
        1 => (),
        2 => fields.push("extra:optional-u64".into()),
        _ => return Err("unsupported codec schema".into()),
    }
    Ok(Schema {
        version,
        rows: cfg.rows,
        revisions: cfg.revisions,
        fields,
    })
}
#[derive(Serialize, Deserialize)]
struct Event {
    version: u32,
    schema: Option<Schema>,
    facts: Vec<Fact>,
    edges: Vec<Edge>,
}
#[derive(Clone, PartialEq, Message)]
struct ProtoSchema {
    #[prost(uint32, tag = "1")]
    version: u32,
    #[prost(uint64, tag = "2")]
    rows: u64,
    #[prost(uint64, tag = "3")]
    revisions: u64,
    #[prost(string, repeated, tag = "4")]
    fields: Vec<String>,
}
#[derive(Clone, PartialEq, Message)]
struct ProtoFact {
    #[prost(uint64, tag = "1")]
    id: u64,
    #[prost(uint64, tag = "2")]
    revision: u64,
    #[prost(uint32, tag = "3")]
    schema_version: u32,
    #[prost(uint64, tag = "4")]
    action: u64,
    #[prost(uint64, tag = "5")]
    objective: u64,
    #[prost(string, tag = "6")]
    text: String,
    #[prost(uint64, optional, tag = "7")]
    extra: Option<u64>,
}
#[derive(Clone, PartialEq, Message)]
struct ProtoEdge {
    #[prost(uint64, tag = "1")]
    id: u64,
    #[prost(uint64, tag = "2")]
    source: u64,
    #[prost(uint64, tag = "3")]
    kind: u64,
    #[prost(uint64, tag = "4")]
    target: u64,
}
#[derive(Clone, PartialEq, Message)]
struct ProtoEvent {
    #[prost(uint32, tag = "1")]
    version: u32,
    #[prost(message, optional, tag = "2")]
    schema: Option<ProtoSchema>,
    #[prost(message, repeated, tag = "3")]
    facts: Vec<ProtoFact>,
    #[prost(message, repeated, tag = "4")]
    edges: Vec<ProtoEdge>,
}
impl From<&Event> for ProtoEvent {
    fn from(e: &Event) -> Self {
        Self {
            version: e.version,
            schema: e.schema.as_ref().map(|s| ProtoSchema {
                version: s.version,
                rows: s.rows,
                revisions: s.revisions,
                fields: s.fields.clone(),
            }),
            facts: e
                .facts
                .iter()
                .map(|f| ProtoFact {
                    id: f.id,
                    revision: f.revision,
                    schema_version: f.schema_version,
                    action: f.action,
                    objective: f.objective,
                    text: f.text.clone(),
                    extra: f.extra,
                })
                .collect(),
            edges: e
                .edges
                .iter()
                .map(|e| ProtoEdge {
                    id: e.id,
                    source: e.source,
                    kind: e.kind,
                    target: e.target,
                })
                .collect(),
        }
    }
}
impl From<ProtoEvent> for Event {
    fn from(e: ProtoEvent) -> Self {
        Self {
            version: e.version,
            schema: e.schema.map(|s| Schema {
                version: s.version,
                rows: s.rows,
                revisions: s.revisions,
                fields: s.fields,
            }),
            facts: e
                .facts
                .into_iter()
                .map(|f| Fact {
                    id: f.id,
                    revision: f.revision,
                    schema_version: f.schema_version,
                    action: f.action,
                    objective: f.objective,
                    text: f.text,
                    extra: f.extra,
                })
                .collect(),
            edges: e
                .edges
                .into_iter()
                .map(|e| Edge {
                    id: e.id,
                    source: e.source,
                    kind: e.kind,
                    target: e.target,
                })
                .collect(),
        }
    }
}
impl Codec {
    fn encode(self, e: &Event) -> Result<Vec<u8>> {
        Ok(match self {
            Self::MessagePack => rmp_serde::to_vec(e)?,
            Self::Protobuf => ProtoEvent::from(e).encode_to_vec(),
        })
    }
    fn decode(self, bytes: &[u8]) -> Result<Event> {
        Ok(match self {
            Self::MessagePack => rmp_serde::from_slice(bytes)?,
            Self::Protobuf => ProtoEvent::decode(bytes)?.into(),
        })
    }
}
#[derive(Clone, Copy)]
struct Location {
    offset: u64,
    member: usize,
}
#[derive(Clone, Copy)]
struct Head {
    revision: u64,
    schema: u32,
    action: u64,
    objective: u64,
}
#[derive(Clone, Copy)]
struct EdgeLocation {
    edge: Edge,
    location: Location,
}
struct Log {
    codec: Codec,
    cfg: Config,
    store: Store,
    schemas: BTreeSet<u32>,
    history: BTreeMap<(u64, u64), Location>,
    heads: BTreeMap<u64, Head>,
    edges: BTreeMap<u64, EdgeLocation>,
    endpoints: BTreeSet<u64>,
    secondary: BTreeMap<(u64, u64), BTreeSet<u64>>,
    sources: BTreeMap<u64, Vec<u64>>,
    targets: BTreeMap<u64, Vec<u64>>,
    projected: bool,
    payload_bytes: u64,
}
pub fn open_messagepack(path: &Path, new: bool, cfg: Config) -> Result<Box<dyn Backend>> {
    open(path, new, cfg, Codec::MessagePack)
}
pub fn open_protobuf(path: &Path, new: bool, cfg: Config) -> Result<Box<dyn Backend>> {
    open(path, new, cfg, Codec::Protobuf)
}
fn open(path: &Path, new: bool, cfg: Config, codec: Codec) -> Result<Box<dyn Backend>> {
    Ok(Box::new(open_log(path, new, cfg, codec)?))
}
fn open_log(path: &Path, new: bool, cfg: Config, codec: Codec) -> Result<Log> {
    cfg.validate()?;
    let exists = path.join("manifest.cbr").exists();
    if new == exists {
        return Err("codec new/reopen directory state mismatch".into());
    }
    let store = Store::open(path)?;
    let mut log = Log {
        codec,
        cfg,
        store,
        schemas: BTreeSet::new(),
        history: BTreeMap::new(),
        heads: BTreeMap::new(),
        edges: BTreeMap::new(),
        endpoints: BTreeSet::new(),
        secondary: BTreeMap::new(),
        sources: BTreeMap::new(),
        targets: BTreeMap::new(),
        projected: false,
        payload_bytes: 0,
    };
    for entry in log.store.snapshot() {
        let event = codec.decode(&entry.payload)?;
        log.validate(&event)?;
        let offset = log
            .store
            .frame_offset(entry.sequence)
            .ok_or("codec missing frame offset")?;
        log.apply(event, offset, entry.payload.len() as u64);
    }
    if !new && !log.schemas.contains(&1) {
        return Err("codec missing registry".into());
    }
    Ok(log)
}
impl Log {
    fn validate(&self, e: &Event) -> Result<()> {
        if e.version != 1 {
            return Err("codec event version".into());
        }
        let variants = usize::from(e.schema.is_some())
            + usize::from(!e.facts.is_empty())
            + usize::from(!e.edges.is_empty());
        if variants != 1 {
            return Err("codec event shape".into());
        }
        if let Some(s) = &e.schema
            && (*s != schema(self.cfg, s.version)? || self.schemas.contains(&s.version))
        {
            return Err("codec registry definition conflict".into());
        }
        if !e.facts.is_empty() {
            if e.facts.len() > 256 {
                return Err("codec fact batch bound".into());
            }
            let mut ids = BTreeSet::new();
            for f in &e.facts {
                if f.id == 0
                    || f.id > self.cfg.rows
                    || !ids.insert(f.id)
                    || !self.schemas.contains(&f.schema_version)
                    || (f.schema_version == 1 && f.extra.is_some())
                    || f.text.len() > 1024 * 1024
                {
                    return Err("codec invalid fact".into());
                }
                let prior = self.heads.get(&f.id);
                if f.revision != prior.map_or(1, |h| h.revision + 1) {
                    return Err("codec revision conflict".into());
                }
                if prior.is_some_and(|h| h.schema != f.schema_version)
                    && self.endpoints.contains(&f.id)
                {
                    return Err("codec linked schema conflict".into());
                }
            }
        }
        if !e.edges.is_empty() {
            if e.edges.len() > 256 {
                return Err("codec edge batch bound".into());
            }
            let mut ids = BTreeSet::new();
            for edge in &e.edges {
                if edge.id == 0
                    || edge.kind != 1
                    || !ids.insert(edge.id)
                    || self.edges.contains_key(&edge.id)
                {
                    return Err("codec edge identity conflict".into());
                }
                for endpoint in [edge.source, edge.target] {
                    if self.heads.get(&endpoint).map(|h| h.schema) != Some(1) {
                        return Err("codec endpoint schema mismatch".into());
                    }
                }
            }
        }
        Ok(())
    }
    fn apply(&mut self, e: Event, offset: u64, bytes: u64) {
        self.payload_bytes += bytes;
        if let Some(s) = e.schema {
            self.schemas.insert(s.version);
        }
        for (member, f) in e.facts.into_iter().enumerate() {
            self.history
                .insert((f.id, f.revision), Location { offset, member });
            self.heads.insert(
                f.id,
                Head {
                    revision: f.revision,
                    schema: f.schema_version,
                    action: f.action,
                    objective: f.objective,
                },
            );
        }
        for (member, edge) in e.edges.into_iter().enumerate() {
            self.endpoints.insert(edge.source);
            self.endpoints.insert(edge.target);
            self.edges.insert(
                edge.id,
                EdgeLocation {
                    edge,
                    location: Location { offset, member },
                },
            );
        }
        self.projected = false;
    }
    fn append(&mut self, event: Event) -> Result<()> {
        self.validate(&event)?;
        let bytes = self.codec.encode(&event)?;
        let receipt = self.store.append(&bytes)?;
        let offset = self
            .store
            .frame_offset(receipt.sequence)
            .ok_or("codec unpublished frame")?;
        self.apply(event, offset, bytes.len() as u64);
        Ok(())
    }
    fn read_fact(&mut self, id: u64, revision: u64) -> Result<Fact> {
        let loc = *self
            .history
            .get(&(id, revision))
            .ok_or("codec revision missing")?;
        let entry = self.store.read_frame_at(loc.offset)?;
        let event = self.codec.decode(&entry.payload)?;
        let f = event
            .facts
            .into_iter()
            .nth(loc.member)
            .ok_or("codec fact member missing")?;
        if f.id != id || f.revision != revision {
            return Err("codec indexed identity mismatch".into());
        }
        Ok(f)
    }
    fn read_edge(&mut self, id: u64) -> Result<Edge> {
        let loc = *self.edges.get(&id).ok_or("codec edge missing")?;
        let entry = self.store.read_frame_at(loc.location.offset)?;
        let event = self.codec.decode(&entry.payload)?;
        let e = event
            .edges
            .into_iter()
            .nth(loc.location.member)
            .ok_or("codec edge member missing")?;
        if e != loc.edge {
            return Err("codec edge index mismatch".into());
        }
        Ok(e)
    }
    fn ensure_projected(&self) -> Result<()> {
        if !self.projected {
            return Err("codec projection not built".into());
        }
        Ok(())
    }
}
impl Backend for Log {
    fn register_schema(&mut self, version: u32) -> Result<()> {
        let s = schema(self.cfg, version)?;
        if self.schemas.contains(&version) {
            return Ok(());
        }
        self.append(Event {
            version: 1,
            schema: Some(s),
            facts: vec![],
            edges: vec![],
        })
    }
    fn write_facts(&mut self, batch: &[Fact]) -> Result<()> {
        if batch.is_empty() {
            return Err("codec empty fact batch".into());
        }
        self.append(Event {
            version: 1,
            schema: None,
            facts: batch.to_vec(),
            edges: vec![],
        })
    }
    fn write_edges(&mut self, batch: &[Edge]) -> Result<()> {
        if batch.is_empty() {
            return Err("codec empty edge batch".into());
        }
        self.append(Event {
            version: 1,
            schema: None,
            facts: vec![],
            edges: batch.to_vec(),
        })
    }
    fn rejects_linked_schema_change(&mut self, cfg: Config) -> Result<bool> {
        let before = *self.heads.get(&1).ok_or("codec head missing")?;
        let old_frames = self.store.snapshot().len();
        let mut f = common::fact(cfg, 1, before.revision + 1);
        f.schema_version = 2;
        f.extra = Some(3);
        let rejected = self.write_facts(&[f]).is_err();
        Ok(rejected
            && self.heads[&1].revision == before.revision
            && self.store.snapshot().len() == old_frames)
    }
    fn flush(&mut self) -> Result<()> {
        self.store.checkpoint()?;
        Ok(())
    }
    fn project(&mut self) -> Result<()> {
        self.secondary.clear();
        self.sources.clear();
        self.targets.clear();
        for (id, h) in &self.heads {
            self.secondary
                .entry((h.action, h.objective))
                .or_default()
                .insert(*id);
        }
        for (id, e) in &self.edges {
            self.sources.entry(e.edge.source).or_default().push(*id);
            self.targets.entry(e.edge.target).or_default().push(*id);
        }
        self.projected = true;
        Ok(())
    }
    fn filter_facts(&mut self, action: u64, objective: u64) -> Result<Vec<Fact>> {
        let ids = self.filter(action, objective)?;
        ids.into_iter()
            .map(|id| {
                let h = *self.heads.get(&id).ok_or("codec head missing")?;
                let f = self.read_fact(id, h.revision)?;
                if (f.schema_version, f.action, f.objective) != (h.schema, h.action, h.objective) {
                    return Err("codec head/payload mismatch".into());
                }
                Ok(f)
            })
            .collect()
    }
    fn select(
        &mut self,
        action: u64,
        objective: u64,
        shape: ProjectionShape,
    ) -> Result<ProjectionResult> {
        if shape == ProjectionShape::Ids {
            let ids = self.filter(action, objective)?;
            let rows = ids
                .into_iter()
                .map(|id| {
                    let h = self.heads.get(&id).ok_or("codec head missing")?;
                    Ok(Identity {
                        id,
                        revision: h.revision,
                        schema_version: h.schema,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(ProjectionResult::Ids(rows));
        }
        if shape == ProjectionShape::Fields {
            // Decode the physical frame as before, but retain only requested cells per row.
            // This avoids keeping every selected text String alive until a full Vec<Fact> is mapped.
            let ids = self.filter(action, objective)?;
            let fields = ids
                .into_iter()
                .map(|id| {
                    let h = *self.heads.get(&id).ok_or("codec head missing")?;
                    let f = self.read_fact(id, h.revision)?;
                    if (f.schema_version, f.action, f.objective)
                        != (h.schema, h.action, h.objective)
                    {
                        return Err("codec head/payload mismatch".into());
                    }
                    Ok(ProjectedFields {
                        identity: common::identity(&f),
                        action: f.action,
                        objective: f.objective,
                        extra: f.extra,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            return Ok(ProjectionResult::Fields(fields));
        }
        Ok(ProjectionResult::Full(
            self.filter_facts(action, objective)?,
        ))
    }
    fn filter(&mut self, action: u64, objective: u64) -> Result<Vec<u64>> {
        self.ensure_projected()?;
        Ok(self
            .secondary
            .get(&(action, objective))
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default())
    }
    fn walk(&mut self, start: u64, depth: usize, incoming: bool) -> Result<Vec<Edge>> {
        self.ensure_projected()?;
        if depth > 16 {
            return Err("codec walk depth bound".into());
        }
        let mut frontier = vec![start];
        let mut visited = BTreeSet::from([start]);
        let mut out = BTreeMap::new();
        for _ in 0..depth {
            let mut next = Vec::new();
            for node in frontier {
                let ids = if incoming {
                    self.targets.get(&node)
                } else {
                    self.sources.get(&node)
                }
                .cloned()
                .unwrap_or_default();
                for id in ids {
                    let edge = self.read_edge(id)?;
                    let target = if incoming { edge.source } else { edge.target };
                    out.insert(edge.id, edge);
                    if visited.insert(target) {
                        next.push(target);
                    }
                }
            }
            frontier = next;
        }
        Ok(out.into_values().collect())
    }
    fn history(&mut self, id: u64, first: u64, last: u64) -> Result<Vec<Fact>> {
        if first == 0 || first > last || last - first >= 100 {
            return Err("codec history bounds".into());
        }
        (first..=last).map(|r| self.read_fact(id, r)).collect()
    }
    fn json(&mut self, id: u64) -> Result<Vec<u8>> {
        let revision = self.heads.get(&id).ok_or("codec head missing")?.revision;
        let f = self.read_fact(id, revision)?;
        Ok(serde_json::to_vec(&common::canonical_json(&f))?)
    }
    fn scan(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        for entry in self.store.snapshot() {
            let event = self.codec.decode(&entry.payload)?;
            for f in &event.facts {
                visitor(f)?;
            }
        }
        Ok(())
    }
    fn scan_heads(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        let mut emitted = BTreeSet::new();
        // Decode each canonical frame once; select by the actual head projection,
        // never by generated expected data or by simply choosing the newest log member.
        for entry in self.store.snapshot() {
            let event = self.codec.decode(&entry.payload)?;
            for fact in &event.facts {
                let Some(head) = self.heads.get(&fact.id) else {
                    continue;
                };
                if head.revision != fact.revision {
                    continue;
                }
                if (head.schema, head.action, head.objective)
                    != (fact.schema_version, fact.action, fact.objective)
                {
                    return Err("codec current-head metadata disagrees with stored fact".into());
                }
                if !emitted.insert(fact.id) {
                    return Err("codec duplicate current-head member".into());
                }
                visitor(fact)?;
            }
        }
        if emitted.len() != self.heads.len() {
            return Err("codec current head points to a missing revision".into());
        }
        Ok(())
    }
    fn scan_edges(&mut self, visitor: &mut dyn FnMut(Edge) -> Result<()>) -> Result<()> {
        for entry in self.store.snapshot() {
            let event = self.codec.decode(&entry.payload)?;
            for e in event.edges {
                visitor(e)?;
            }
        }
        Ok(())
    }
    fn details(&self) -> BTreeMap<String, serde_json::Value> {
        let mut d = BTreeMap::new();
        d.insert(
            "payload_bytes".into(),
            serde_json::json!(self.payload_bytes),
        );
        d.insert("payload_scope".into(),serde_json::json!("all encoded logical event payloads including both schemas, fact batches and edge batches; excludes outer CBR framing"));
        d.insert(
            "history_location_entries".into(),
            serde_json::json!(self.history.len()),
        );
        d.insert("head_entries".into(), serde_json::json!(self.heads.len()));
        d.insert("edge_entries".into(), serde_json::json!(self.edges.len()));
        d.insert(
            "secondary_memberships".into(),
            serde_json::json!(self.secondary.values().map(BTreeSet::len).sum::<usize>()),
        );
        d.insert(
            "adjacency_memberships".into(),
            serde_json::json!(
                self.sources.values().map(Vec::len).sum::<usize>()
                    + self.targets.values().map(Vec::len).sum::<usize>()
            ),
        );
        d.insert("indices".into(),serde_json::json!("RAM BTreeMap/BTreeSet location/head/filter/adjacency projections rebuilt from canonical log; no persisted second payload per access paradigm; Store also retains raw journal payloads in RAM"));
        d.insert("durability".into(),serde_json::json!("shared canonical-storage CBR frames/checksum/sync_all, one frame per supplied batch; checkpoint hint, full replay"));
        d.insert("scope".into(),serde_json::json!("functional codec harness, not mature standalone DB or all engine guarantees; exact history/linked-schema checks for declared workload"));
        let io = self.store.io_stats();
        d.insert("point_reads".into(), serde_json::json!(io.point_reads));
        d.insert("point_bytes".into(), serde_json::json!(io.point_bytes));
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory(label: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "functional-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&p).unwrap();
        p
    }
    fn exercise(factory: crate::common::OpenBackend, label: &str) {
        let cfg = Config {
            rows: 16,
            revisions: 11,
            samples: 2,
        };
        let path = directory(label);
        let mut b = factory(&path, true, cfg).unwrap();
        b.register_schema(1).unwrap();
        for rev in 1..=cfg.revisions {
            b.write_facts(
                &(1..=cfg.rows)
                    .map(|id| crate::common::fact(cfg, id, rev))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
        b.write_edges(
            &(1..cfg.graph_nodes())
                .map(crate::common::edge)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        b.register_schema(2).unwrap();
        assert!(b.rejects_linked_schema_change(cfg).unwrap());
        b.write_facts(
            &(cfg.graph_nodes() + 1..=cfg.rows)
                .map(|id| crate::common::fact(cfg, id, 12))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        b.flush().unwrap();
        drop(b);
        let mut b = factory(&path, false, cfg).unwrap();
        b.project().unwrap();
        assert_eq!(b.filter(2, 2).unwrap(), crate::common::matches(cfg, 2, 2));
        assert_eq!(
            b.walk(1, 3, false).unwrap(),
            crate::common::walk(cfg, 1, 3, false)
        );
        assert_eq!(
            b.walk(7, 3, true).unwrap(),
            crate::common::walk(cfg, 7, 3, true)
        );
        assert_eq!(
            b.history(16, 2, 11).unwrap(),
            (2..=11)
                .map(|rev| crate::common::fact(cfg, 16, rev))
                .collect::<Vec<_>>()
        );
        let parsed: serde_json::Value = serde_json::from_slice(&b.json(16).unwrap()).unwrap();
        assert_eq!(
            parsed,
            crate::common::canonical_json(&crate::common::fact(cfg, 16, 12))
        );
        let valid = crate::common::fact(cfg, 16, 13);
        let invalid = crate::common::fact(cfg, 1, 999);
        assert!(b.write_facts(&[valid, invalid]).is_err());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&b.json(16).unwrap()).unwrap(),
            crate::common::canonical_json(&crate::common::fact(cfg, 16, 12))
        );
        let mut heads = 0;
        b.scan_heads(&mut |f| {
            assert_eq!(*f, crate::common::fact(cfg, f.id, cfg.latest(f.id)));
            heads += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(heads, cfg.rows);
        let mut count = 0;
        b.scan(&mut |f| {
            assert_eq!(*f, crate::common::fact(cfg, f.id, f.revision));
            count += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(count, cfg.facts());
        let mut edges = 0;
        b.scan_edges(&mut |e| {
            assert_eq!(e, crate::common::edge(e.id));
            edges += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(edges, 7);
        assert!(b.history(1, 0, 1).is_err());
        drop(b);
    }
    #[test]
    fn messagepack_functional_roundtrip() {
        exercise(open_messagepack, "messagepack");
    }
    #[test]
    fn protobuf_functional_roundtrip() {
        exercise(open_protobuf, "protobuf");
    }
    #[test]
    fn current_head_scan_exposes_stale_unsampled_head_for_both_codecs() {
        for codec in [Codec::MessagePack, Codec::Protobuf] {
            let cfg = Config {
                rows: 16,
                revisions: 11,
                samples: 1,
            };
            let path = directory("codec-stale-head");
            let mut b = open_log(&path, true, cfg, codec).unwrap();
            b.register_schema(1).unwrap();
            for revision in 1..=cfg.revisions {
                b.write_facts(
                    &(1..=cfg.rows)
                        .map(|id| common::fact(cfg, id, revision))
                        .collect::<Vec<_>>(),
                )
                .unwrap();
            }
            b.register_schema(2).unwrap();
            b.write_facts(
                &(9..=16)
                    .map(|id| common::fact(cfg, id, 12))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let head = b.heads.get_mut(&16).unwrap();
            head.revision = 11;
            head.schema = 1;
            let mut saw_stale = false;
            let result = b.scan_heads(&mut |f| {
                if f.id == 16 {
                    saw_stale = true;
                    assert_eq!(f.revision, 11);
                }
                if f.revision != cfg.latest(f.id) {
                    return Err("independent current-head oracle rejected stale head".into());
                }
                Ok(())
            });
            assert!(saw_stale);
            assert!(result.is_err());
            let head = b.heads.get_mut(&16).unwrap();
            head.revision = 12;
            head.schema = 2;
            head.action = 999;
            assert!(
                b.scan_heads(&mut |_| Ok(())).is_err(),
                "corrupt head metadata must not be hidden by valid canonical payload"
            );
        }
    }
}
