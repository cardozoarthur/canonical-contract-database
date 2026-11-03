//! Typed SQLite equivalent for this workload, not an implementation of every engine contract.
use crate::common::{
    self, Backend, Config, Edge, Fact, Identity, ProjectedFields, ProjectionResult,
    ProjectionShape, Result,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
struct Sqlite {
    db: Connection,
    cfg: Config,
}
pub fn open(path: &Path, new: bool, cfg: Config) -> Result<Box<dyn Backend>> {
    cfg.validate()?;
    let file = path.join("functional.sqlite");
    if new && file.exists() {
        return Err("SQLite destination exists".into());
    }
    if !new && !file.is_file() {
        return Err("SQLite reopen source missing".into());
    }
    let db = Connection::open(file)?;
    db.execute_batch("PRAGMA journal_mode=DELETE;PRAGMA synchronous=FULL;PRAGMA foreign_keys=ON;")?;
    if new {
        db.execute_batch("CREATE TABLE registry(version INTEGER PRIMARY KEY,definition TEXT NOT NULL);CREATE TABLE workload(rows INTEGER NOT NULL,revisions INTEGER NOT NULL);CREATE TABLE facts(id INTEGER NOT NULL,revision INTEGER NOT NULL,schema_version INTEGER NOT NULL,action INTEGER NOT NULL,objective INTEGER NOT NULL,text TEXT NOT NULL,extra INTEGER,PRIMARY KEY(id,revision),FOREIGN KEY(schema_version) REFERENCES registry(version)) WITHOUT ROWID;CREATE TABLE heads(id INTEGER PRIMARY KEY,revision INTEGER NOT NULL,schema_version INTEGER NOT NULL,action INTEGER NOT NULL,objective INTEGER NOT NULL);CREATE INDEX heads_action_objective ON heads(action,objective,id);CREATE TABLE edges(id INTEGER PRIMARY KEY,source INTEGER NOT NULL,kind INTEGER NOT NULL,target INTEGER NOT NULL,FOREIGN KEY(source) REFERENCES heads(id),FOREIGN KEY(target) REFERENCES heads(id));CREATE INDEX edges_source ON edges(source,kind,id);CREATE INDEX edges_target ON edges(target,kind,id);")?;
        db.execute(
            "INSERT INTO workload VALUES(?1,?2)",
            params![cfg.rows, cfg.revisions],
        )?;
    } else {
        let actual: (u64, u64) = db.query_row("SELECT rows,revisions FROM workload", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        if actual != (cfg.rows, cfg.revisions) {
            return Err("SQLite workload mismatch".into());
        }
        let check: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err("SQLite quick_check failed".into());
        }
    }
    Ok(Box::new(Sqlite { db, cfg }))
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Fact> {
    Ok(Fact {
        id: r.get(0)?,
        revision: r.get(1)?,
        schema_version: r.get(2)?,
        action: r.get(3)?,
        objective: r.get(4)?,
        text: r.get(5)?,
        extra: r.get(6)?,
    })
}
impl Sqlite {
    fn latest(&self, id: u64) -> Result<Fact> {
        Ok(self.db.query_row("SELECT f.id,f.revision,f.schema_version,f.action,f.objective,f.text,f.extra FROM facts f JOIN heads h ON f.id=h.id AND f.revision=h.revision WHERE f.id=?1",[id],row)?)
    }
    fn validate_batch(&self, batch: &[Fact]) -> Result<()> {
        if batch.is_empty() || batch.len() > 256 {
            return Err("SQLite batch bounds".into());
        }
        let mut keys = BTreeSet::new();
        for f in batch {
            if f.id == 0
                || f.id > self.cfg.rows
                || !keys.insert(f.id)
                || f.schema_version == 0
                || f.schema_version > 2
                || (f.schema_version == 1 && f.extra.is_some())
            {
                return Err("invalid SQLite fact".into());
            }
            let registered: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM registry WHERE version=?1)",
                [f.schema_version],
                |r| r.get(0),
            )?;
            if !registered {
                return Err("unknown SQLite schema".into());
            }
            let previous: Option<(u64, u32)> = self
                .db
                .query_row(
                    "SELECT revision,schema_version FROM heads WHERE id=?1",
                    [f.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if f.revision != previous.map_or(1, |v| v.0 + 1) {
                return Err("SQLite revision conflict".into());
            }
            if previous.is_some_and(|v| v.1 != f.schema_version) {
                let linked: bool = self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM edges WHERE source=?1 OR target=?1)",
                    [f.id],
                    |r| r.get(0),
                )?;
                if linked {
                    return Err("SQLite linked schema conflict".into());
                }
            }
        }
        Ok(())
    }
}
impl Backend for Sqlite {
    fn register_schema(&mut self, version: u32) -> Result<()> {
        let def = match version {
            1 => "id:u64,revision:u64,schema_version:u32,action:u64,objective:u64,text:utf8",
            2 => {
                "id:u64,revision:u64,schema_version:u32,action:u64,objective:u64,text:utf8,extra:optional-u64"
            }
            _ => return Err("unsupported SQLite schema".into()),
        };
        let prior: Option<String> = self
            .db
            .query_row(
                "SELECT definition FROM registry WHERE version=?1",
                [version],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            if prior != def {
                return Err("SQLite schema conflict".into());
            }
            return Ok(());
        }
        self.db
            .execute("INSERT INTO registry VALUES(?1,?2)", params![version, def])?;
        Ok(())
    }
    fn write_facts(&mut self, batch: &[Fact]) -> Result<()> {
        self.validate_batch(batch)?;
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO facts VALUES(?1,?2,?3,?4,?5,?6,?7)")?;
            let mut head=tx.prepare_cached("INSERT INTO heads VALUES(?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,schema_version=excluded.schema_version,action=excluded.action,objective=excluded.objective")?;
            for f in batch {
                insert.execute(params![
                    f.id,
                    f.revision,
                    f.schema_version,
                    f.action,
                    f.objective,
                    f.text,
                    f.extra
                ])?;
                head.execute(params![
                    f.id,
                    f.revision,
                    f.schema_version,
                    f.action,
                    f.objective
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn write_edges(&mut self, batch: &[Edge]) -> Result<()> {
        if batch.is_empty() || batch.len() > 256 {
            return Err("SQLite edge batch bounds".into());
        }
        let mut ids = BTreeSet::new();
        for e in batch {
            if e.id == 0 || e.kind != 1 || !ids.insert(e.id) {
                return Err("invalid SQLite edge".into());
            }
            for endpoint in [e.source, e.target] {
                let schema: u32 = self.db.query_row(
                    "SELECT schema_version FROM heads WHERE id=?1",
                    [endpoint],
                    |r| r.get(0),
                )?;
                if schema != 1 {
                    return Err("SQLite endpoint schema mismatch".into());
                }
            }
        }
        let tx = self.db.transaction()?;
        {
            let mut insert = tx.prepare_cached("INSERT INTO edges VALUES(?1,?2,?3,?4)")?;
            for e in batch {
                insert.execute(params![e.id, e.source, e.kind, e.target])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn rejects_linked_schema_change(&mut self, cfg: Config) -> Result<bool> {
        let before = self.latest(1)?;
        let mut invalid = common::fact(cfg, 1, before.revision + 1);
        invalid.schema_version = 2;
        invalid.extra = Some(3);
        let rejected = self.write_facts(&[invalid]).is_err();
        Ok(rejected && self.latest(1)? == before)
    }
    fn flush(&mut self) -> Result<()> {
        self.db.cache_flush()?;
        Ok(())
    }
    fn project(&mut self) -> Result<()> {
        // B-tree projections are maintained durably during writes.
        let _: u64 = self
            .db
            .query_row("SELECT count(*) FROM heads", [], |r| r.get(0))?;
        Ok(())
    }
    fn filter_facts(&mut self, action: u64, objective: u64) -> Result<Vec<Fact>> {
        let mut stmt=self.db.prepare_cached("SELECT f.id,f.revision,f.schema_version,f.action,f.objective,f.text,f.extra FROM heads h JOIN facts f ON f.id=h.id AND f.revision=h.revision WHERE h.action=?1 AND h.objective=?2 ORDER BY h.id")?;
        Ok(stmt
            .query_map(params![action, objective], row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    fn select(
        &mut self,
        action: u64,
        objective: u64,
        shape: ProjectionShape,
    ) -> Result<ProjectionResult> {
        match shape {
            ProjectionShape::Full => Ok(ProjectionResult::Full(
                self.filter_facts(action, objective)?,
            )),
            ProjectionShape::Ids => {
                let mut stmt=self.db.prepare_cached("SELECT id,revision,schema_version FROM heads WHERE action=?1 AND objective=?2 ORDER BY id")?;
                let rows = stmt
                    .query_map(params![action, objective], |r| {
                        Ok(Identity {
                            id: r.get(0)?,
                            revision: r.get(1)?,
                            schema_version: r.get(2)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(ProjectionResult::Ids(rows))
            }
            ProjectionShape::Fields => {
                let mut stmt=self.db.prepare_cached("SELECT h.id,h.revision,h.schema_version,h.action,h.objective,f.extra FROM heads h JOIN facts f ON f.id=h.id AND f.revision=h.revision WHERE h.action=?1 AND h.objective=?2 ORDER BY h.id")?;
                let rows = stmt
                    .query_map(params![action, objective], |r| {
                        Ok(ProjectedFields {
                            identity: Identity {
                                id: r.get(0)?,
                                revision: r.get(1)?,
                                schema_version: r.get(2)?,
                            },
                            action: r.get(3)?,
                            objective: r.get(4)?,
                            extra: r.get(5)?,
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(ProjectionResult::Fields(rows))
            }
        }
    }
    fn filter(&mut self, action: u64, objective: u64) -> Result<Vec<u64>> {
        let mut stmt=self.db.prepare_cached("SELECT id FROM heads INDEXED BY heads_action_objective WHERE action=?1 AND objective=?2 ORDER BY id")?;
        Ok(stmt
            .query_map(params![action, objective], |r| r.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }
    fn walk(&mut self, start: u64, depth: usize, incoming: bool) -> Result<Vec<Edge>> {
        if depth > 16 {
            return Err("SQLite walk depth bound".into());
        }
        let sql = if incoming {
            "SELECT id,source,kind,target FROM edges INDEXED BY edges_target WHERE target=?1 ORDER BY id"
        } else {
            "SELECT id,source,kind,target FROM edges INDEXED BY edges_source WHERE source=?1 ORDER BY id"
        };
        let mut frontier = vec![start];
        let mut visited = BTreeSet::from([start]);
        let mut out = BTreeMap::new();
        for _ in 0..depth {
            let mut next = Vec::new();
            for node in frontier {
                let mut stmt = self.db.prepare_cached(sql)?;
                for e in stmt.query_map([node], |r| {
                    Ok(Edge {
                        id: r.get(0)?,
                        source: r.get(1)?,
                        kind: r.get(2)?,
                        target: r.get(3)?,
                    })
                })? {
                    let e = e?;
                    let target = if incoming { e.source } else { e.target };
                    out.insert(e.id, e);
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
            return Err("SQLite history bounds".into());
        }
        let mut stmt=self.db.prepare_cached("SELECT id,revision,schema_version,action,objective,text,extra FROM facts WHERE id=?1 AND revision BETWEEN ?2 AND ?3 ORDER BY revision")?;
        let found = stmt
            .query_map(params![id, first, last], row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if found.len() as u64 != last - first + 1 {
            return Err("SQLite requested revision missing".into());
        }
        Ok(found)
    }
    fn json(&mut self, id: u64) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&common::canonical_json(
            &self.latest(id)?,
        ))?)
    }
    fn scan(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        let mut stmt=self.db.prepare("SELECT id,revision,schema_version,action,objective,text,extra FROM facts ORDER BY id,revision")?;
        for f in stmt.query_map([], row)? {
            visitor(&f?)?;
        }
        Ok(())
    }
    fn scan_heads(&mut self, visitor: &mut dyn FnMut(&Fact) -> Result<()>) -> Result<()> {
        let mut stmt = self.db.prepare("SELECT f.id,f.revision,f.schema_version,f.action,f.objective,f.text,f.extra,h.id,h.revision,h.schema_version,h.action,h.objective FROM heads h LEFT JOIN facts f ON f.id=h.id AND f.revision=h.revision ORDER BY h.id")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let fact = row(r)?; // A dangling head yields NULL and is rejected here.
            let metadata = (
                r.get::<_, u64>(7)?,
                r.get::<_, u64>(8)?,
                r.get::<_, u32>(9)?,
                r.get::<_, u64>(10)?,
                r.get::<_, u64>(11)?,
            );
            if metadata
                != (
                    fact.id,
                    fact.revision,
                    fact.schema_version,
                    fact.action,
                    fact.objective,
                )
            {
                return Err("SQLite current-head metadata disagrees with stored fact".into());
            }
            visitor(&fact)?;
        }
        Ok(())
    }
    fn scan_edges(&mut self, visitor: &mut dyn FnMut(Edge) -> Result<()>) -> Result<()> {
        let mut stmt = self
            .db
            .prepare("SELECT id,source,kind,target FROM edges ORDER BY id")?;
        for e in stmt.query_map([], |r| {
            Ok(Edge {
                id: r.get(0)?,
                source: r.get(1)?,
                kind: r.get(2)?,
                target: r.get(3)?,
            })
        })? {
            visitor(e?)?;
        }
        Ok(())
    }
    fn details(&self) -> BTreeMap<String, serde_json::Value> {
        let payload = self
            .db
            .query_row(
                "SELECT sum(payload) FROM dbstat WHERE name IN ('facts','edges','registry')",
                [],
                |r| r.get::<_, Option<u64>>(0),
            )
            .ok()
            .flatten();
        let mut d = BTreeMap::new();
        let plans = [
            ("filter", "EXPLAIN QUERY PLAN SELECT id FROM heads INDEXED BY heads_action_objective WHERE action=2 AND objective=2 ORDER BY id"),
            ("outgoing", "EXPLAIN QUERY PLAN SELECT id,source,kind,target FROM edges INDEXED BY edges_source WHERE source=1 ORDER BY id"),
            ("incoming", "EXPLAIN QUERY PLAN SELECT id,source,kind,target FROM edges INDEXED BY edges_target WHERE target=8 ORDER BY id"),
        ].into_iter().map(|(label,sql)| {
            let plan=self.db.prepare(sql).and_then(|mut stmt|stmt.query_map([],|r|r.get::<_,String>(3))?.collect::<rusqlite::Result<Vec<_>>>());
            (label.to_string(), match plan {Ok(plan)=>serde_json::json!(plan),Err(e)=>serde_json::json!({"unavailable":e.to_string()})})
        }).collect::<BTreeMap<_,_>>();
        d.insert("query_plans".into(), serde_json::json!(plans));
        let index_pages = self
            .db
            .prepare("SELECT name,sum(pgsize),sum(payload) FROM dbstat GROUP BY name ORDER BY name")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, u64>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
            });
        d.insert(
            "dbstat_components".into(),
            match index_pages {
                Ok(pages) => serde_json::json!(pages),
                Err(e) => serde_json::json!({"unavailable":e.to_string()}),
            },
        );
        d.insert("payload_bytes".into(), serde_json::json!(payload));
        d.insert("payload_scope".into(),serde_json::json!("SQLite dbstat payload cells for facts+edges+registry if dbstat available; excludes indices, not an external serialization size"));
        d.insert("indices".into(),serde_json::json!("durable B-trees: history PK, heads projection+action/objective, edge source/target; cost maintained during writes"));
        d.insert(
            "durability".into(),
            serde_json::json!(
                "SQLite synchronous FULL, journal DELETE, transaction per supplied batch"
            ),
        );
        d.insert("scope".into(),serde_json::json!("typed SQLite workload model; history payload once in facts, heads repeats only index fields; schema evolution on unlinked half; no claim of all engine guarantees"));
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
    fn sqlite_functional_roundtrip() {
        exercise(open, "sqlite");
    }
    #[test]
    fn current_head_scan_exposes_stale_unsampled_head_from_disk() {
        let cfg = Config {
            rows: 16,
            revisions: 11,
            samples: 1,
        };
        let path = directory("sqlite-stale-head");
        let mut b = open(&path, true, cfg).unwrap();
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
        drop(b);
        let db = Connection::open(path.join("functional.sqlite")).unwrap();
        db.execute(
            "UPDATE heads SET revision=11,schema_version=1 WHERE id=16",
            [],
        )
        .unwrap();
        drop(db);
        let mut b = open(&path, false, cfg).unwrap();
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
        drop(b);
        let db = Connection::open(path.join("functional.sqlite")).unwrap();
        db.execute(
            "UPDATE heads SET revision=12,schema_version=2,action=999 WHERE id=16",
            [],
        )
        .unwrap();
        drop(db);
        let mut b = open(&path, false, cfg).unwrap();
        assert!(
            b.scan_heads(&mut |_| Ok(())).is_err(),
            "corrupt head metadata must not be hidden by valid canonical payload"
        );
    }
}
