//! Fixed workload contract; changes require coordination across backends.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, error::Error, path::Path};
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Config {
    pub rows: u64,
    pub revisions: u64,
    pub samples: usize,
}
impl Config {
    pub fn graph_nodes(self) -> u64 {
        self.rows / 2
    }
    pub fn latest(self, id: u64) -> u64 {
        self.revisions + u64::from(id > self.graph_nodes())
    }
    pub fn facts(self) -> u64 {
        self.rows * self.revisions + self.rows - self.graph_nodes()
    }
    pub fn validate(self) -> Result<()> {
        if self.rows < 8
            || self.rows > 100_000
            || self.revisions < 11
            || self.revisions > 20
            || self.samples < 1
            || self.samples > 1000
        {
            return Err(
                "invalid workload bounds: rows8..100000/revisions11..20/samples1..1000".into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fact {
    pub id: u64,
    pub revision: u64,
    pub schema_version: u32,
    pub action: u64,
    pub objective: u64,
    pub text: String,
    pub extra: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
    pub id: u64,
    pub source: u64,
    pub kind: u64,
    pub target: u64,
}
pub fn fact(cfg: Config, id: u64, revision: u64) -> Fact {
    let migrated = revision > cfg.revisions;
    Fact {
        id,
        revision,
        schema_version: if migrated { 2 } else { 1 },
        action: 1 + id % 7,
        objective: 1 + id % 17,
        text: format!("Evidence {id}/{revision}: ação determinística"),
        extra: if migrated { Some(id * 3) } else { None },
    }
}
pub fn edge(id: u64) -> Edge {
    Edge {
        id,
        source: id,
        kind: 1,
        target: id + 1,
    }
}
pub fn matches(cfg: Config, action: u64, objective: u64) -> Vec<u64> {
    (1..=cfg.rows)
        .filter(|id| 1 + id % 7 == action && 1 + id % 17 == objective)
        .collect()
}
pub fn walk(cfg: Config, start: u64, depth: usize, incoming: bool) -> Vec<Edge> {
    let mut out = Vec::new();
    let mut node = start;
    for _ in 0..depth {
        if incoming {
            if node <= 1 || node > cfg.graph_nodes() {
                break;
            }
            out.push(edge(node - 1));
            node -= 1;
        } else {
            if node == 0 || node >= cfg.graph_nodes() {
                break;
            }
            out.push(edge(node));
            node += 1;
        }
    }
    out.sort();
    out
}
pub fn canonical_json(f: &Fact) -> serde_json::Value {
    let mut fields = serde_json::json!({"action":f.action,"objective":f.objective,"text":f.text});
    if f.schema_version == 2 {
        fields["extra"] = serde_json::json!(f.extra);
    }
    serde_json::json!({"namespace":1,"id":f.id,"schema":{"namespace":1,"id":1,"version":f.schema_version},"revision":f.revision,"fields":fields})
}
/// Each backend owns persistence and derived indices. scan visits each revision exactly once,
/// order unspecified. Graph edges are separate canonical records, never duplicate fact payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionShape {
    Ids,
    Fields,
    Full,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub id: u64,
    pub revision: u64,
    pub schema_version: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedFields {
    pub identity: Identity,
    pub action: u64,
    pub objective: u64,
    pub extra: Option<u64>,
}
/// Namespace=1/schema identity=(1,1) for this workload. schema_version=1 => extra absent;
/// schema_version=2 with extra=None => optional null. These states must never be collapsed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectionResult {
    Ids(Vec<Identity>),
    Fields(Vec<ProjectedFields>),
    Full(Vec<Fact>),
}
pub fn identity(f: &Fact) -> Identity {
    Identity {
        id: f.id,
        revision: f.revision,
        schema_version: f.schema_version,
    }
}
pub fn project_facts(facts: Vec<Fact>, shape: ProjectionShape) -> ProjectionResult {
    match shape {
        ProjectionShape::Ids => ProjectionResult::Ids(facts.iter().map(identity).collect()),
        ProjectionShape::Fields => ProjectionResult::Fields(
            facts
                .iter()
                .map(|f| ProjectedFields {
                    identity: identity(f),
                    action: f.action,
                    objective: f.objective,
                    extra: f.extra,
                })
                .collect(),
        ),
        ProjectionShape::Full => ProjectionResult::Full(facts),
    }
}
pub trait Backend {
    fn register_schema(&mut self, version: u32) -> Result<()>;
    fn write_facts(&mut self, batch: &[Fact]) -> Result<()>;
    fn write_edges(&mut self, batch: &[Edge]) -> Result<()>;
    fn rejects_linked_schema_change(&mut self, cfg: Config) -> Result<bool>;
    fn flush(&mut self) -> Result<()>;
    /// Include all construction of secondary/adjacency projections needed by following calls.
    fn project(&mut self) -> Result<()>;
    /// Engine-only legacy-shaped Record materialization baseline; no payload decode.
    fn filter_record_identities(&mut self, _action: u64, _objective: u64) -> Result<Vec<Identity>> {
        Err("record baseline unsupported".into())
    }
    fn filter_facts(&mut self, action: u64, objective: u64) -> Result<Vec<Fact>>;
    fn select(
        &mut self,
        action: u64,
        objective: u64,
        shape: ProjectionShape,
    ) -> Result<ProjectionResult>;
    fn filter(&mut self, action: u64, objective: u64) -> Result<Vec<u64>>;
    fn walk(&mut self, start: u64, depth: usize, incoming: bool) -> Result<Vec<Edge>>;
    /// Inclusive endpoints, ascending revision order; must read actual stored revisions.
    fn history(&mut self, id: u64, first: u64, last: u64) -> Result<Vec<Fact>>;
    fn json(&mut self, id: u64) -> Result<Vec<u8>>;
    fn scan(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()>;
    fn scan_heads(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()>;
    fn scan_edges(&mut self, visitor: &mut dyn FnMut(Edge) -> Result<()>) -> Result<()>;
    fn details(&self) -> BTreeMap<String, serde_json::Value>;
}
pub type OpenBackend = fn(&Path, bool, Config) -> Result<Box<dyn Backend>>;
