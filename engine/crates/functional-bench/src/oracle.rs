use crate::common::*;
use sha2::{Digest, Sha256};
pub struct Verifier {
    cfg: Config,
    seen: Vec<u8>,
    count: u64,
    digest: [u8; 32],
}
impl Verifier {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            seen: vec![0; (cfg.rows * (cfg.revisions + 1)).div_ceil(8) as usize],
            count: 0,
            digest: [0; 32],
        }
    }
    pub fn observe(&mut self, f: &Fact) -> Result<()> {
        if f.id == 0
            || f.id > self.cfg.rows
            || f.revision == 0
            || f.revision > self.cfg.latest(f.id)
            || *f != fact(self.cfg, f.id, f.revision)
        {
            return Err("stored fact differs from exact deterministic oracle".into());
        }
        let i = ((f.id - 1) * (self.cfg.revisions + 1) + f.revision - 1) as usize;
        let mask = 1 << (i % 8);
        if self.seen[i / 8] & mask != 0 {
            return Err("duplicate stored revision".into());
        }
        self.seen[i / 8] |= mask;
        self.count += 1;
        let hash = Sha256::digest(serde_json::to_vec(f)?);
        for (to, from) in self.digest.iter_mut().zip(hash) {
            *to ^= from;
        }
        Ok(())
    }
    pub fn finish(self) -> Result<String> {
        if self.count != self.cfg.facts() {
            return Err("incomplete logical history".into());
        }
        Ok(format!("{}:{}", self.count, hex(&self.digest)))
    }
}
pub struct EdgeVerifier {
    cfg: Config,
    seen: Vec<bool>,
    count: u64,
    digest: [u8; 32],
}
impl EdgeVerifier {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            seen: vec![false; cfg.graph_nodes() as usize],
            count: 0,
            digest: [0; 32],
        }
    }
    pub fn observe(&mut self, e: Edge) -> Result<()> {
        if e.id == 0 || e.id >= self.cfg.graph_nodes() || e != edge(e.id) {
            return Err("edge differs from exact oracle".into());
        }
        if self.seen[e.id as usize] {
            return Err("duplicate stored edge".into());
        }
        self.seen[e.id as usize] = true;
        self.count += 1;
        for (to, from) in self
            .digest
            .iter_mut()
            .zip(Sha256::digest(serde_json::to_vec(&e)?))
        {
            *to ^= from;
        }
        Ok(())
    }
    pub fn finish(self) -> Result<String> {
        if self.count != self.cfg.graph_nodes() - 1 {
            return Err("incomplete graph".into());
        }
        Ok(format!("{}:{}", self.count, hex(&self.digest)))
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub struct HeadVerifier {
    cfg: Config,
    seen: Vec<bool>,
    count: u64,
    digest: [u8; 32],
}
impl HeadVerifier {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            seen: vec![false; cfg.rows as usize],
            count: 0,
            digest: [0; 32],
        }
    }
    pub fn observe(&mut self, f: &Fact) -> Result<()> {
        if f.id == 0
            || f.id > self.cfg.rows
            || f.revision != self.cfg.latest(f.id)
            || *f != fact(self.cfg, f.id, self.cfg.latest(f.id))
        {
            return Err("current head is stale or differs from exact oracle".into());
        }
        let index = (f.id - 1) as usize;
        if self.seen[index] {
            return Err("duplicate current head".into());
        }
        self.seen[index] = true;
        self.count += 1;
        for (to, from) in self
            .digest
            .iter_mut()
            .zip(Sha256::digest(serde_json::to_vec(f)?))
        {
            *to ^= from;
        }
        Ok(())
    }
    pub fn finish(self) -> Result<String> {
        if self.count != self.cfg.rows {
            return Err("missing current heads".into());
        }
        Ok(format!("{}:{}", self.count, hex(&self.digest)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn cfg() -> Config {
        Config {
            rows: 8,
            revisions: 11,
            samples: 1,
        }
    }
    #[test]
    fn exact_row_verifier_rejects_mutation_duplicate_and_missing_history() {
        let c = cfg();
        let mut v = Verifier::new(c);
        let f = fact(c, 1, 1);
        v.observe(&f).unwrap();
        assert!(v.observe(&f).is_err());
        let mut f = f;
        f.action += 1;
        assert!(Verifier::new(c).observe(&f).is_err());
        assert!(Verifier::new(c).finish().is_err());
    }
    #[test]
    fn complete_history_is_verified_independently_of_scan_order() {
        let c = cfg();
        let mut forward = Verifier::new(c);
        let mut reverse = Verifier::new(c);
        for id in 1..=c.rows {
            for rev in 1..=c.latest(id) {
                forward.observe(&fact(c, id, rev)).unwrap();
            }
        }
        for id in (1..=c.rows).rev() {
            for rev in (1..=c.latest(id)).rev() {
                reverse.observe(&fact(c, id, rev)).unwrap();
            }
        }
        assert_eq!(forward.finish().unwrap(), reverse.finish().unwrap());
    }
}

#[cfg(test)]
mod head_tests {
    use super::*;
    #[test]
    fn every_head_requires_maximum_revision_and_complete_unique_identity_set() {
        let cfg = Config {
            rows: 8,
            revisions: 11,
            samples: 1,
        };
        assert!(HeadVerifier::new(cfg).observe(&fact(cfg, 1, 1)).is_err());
        assert!(HeadVerifier::new(cfg).finish().is_err());
        let mut v = HeadVerifier::new(cfg);
        for id in 1..=cfg.rows {
            v.observe(&fact(cfg, id, cfg.latest(id))).unwrap();
        }
        assert!(v.observe(&fact(cfg, 1, cfg.latest(1))).is_err());
        assert!(v.finish().is_ok());
    }
}
