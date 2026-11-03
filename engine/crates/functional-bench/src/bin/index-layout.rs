//! Experimental storage layouts only. Never opens or rewrites an engine database.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const MAX_EDGES: usize = 200_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"IDXEXP01";
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Node {
    namespace: u64,
    id: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct Edge {
    id: u64,
    source: Node,
    kind: u64,
    target: Node,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Layout {
    Duplicated,
    References,
    SortedVarint,
    Delta,
}
impl Layout {
    #[cfg(test)]
    const ALL: [Self; 4] = [
        Self::Duplicated,
        Self::References,
        Self::SortedVarint,
        Self::Delta,
    ];
    fn number(self) -> u32 {
        match self {
            Self::Duplicated => 1,
            Self::References => 2,
            Self::SortedVarint => 3,
            Self::Delta => 4,
        }
    }
    fn from_number(n: u32) -> Result<Self> {
        match n {
            1 => Ok(Self::Duplicated),
            2 => Ok(Self::References),
            3 => Ok(Self::SortedVarint),
            4 => Ok(Self::Delta),
            _ => Err("unknown experimental layout".into()),
        }
    }
    fn parse(s: &str) -> Result<Self> {
        match s {
            "duplicated" => Ok(Self::Duplicated),
            "references" => Ok(Self::References),
            "sorted_varint" => Ok(Self::SortedVarint),
            "delta" => Ok(Self::Delta),
            _ => Err("layout must be duplicated/references/sorted_varint/delta".into()),
        }
    }
}
fn node(id: u64) -> Node {
    Node {
        namespace: 1 + id % 3,
        id,
    }
}
fn make_edges(profile: &str, count: usize) -> Result<Vec<Edge>> {
    if count == 0 || count > MAX_EDGES {
        return Err("edge count must be 1..200000".into());
    }
    let mut edges = Vec::with_capacity(count);
    for i in 0..count as u64 {
        let (source, target) = match profile {
            "chain" => (i + 1, i + 2),
            "fanout" => (i / 16 + 1, i + 2),
            "circulant" => {
                let nodes = (count as u64).div_ceil(8);
                let source = i / 8 + 1;
                (source, (source + (i % 8) * 7) % nodes + 1)
            }
            _ => return Err("profile must be chain/fanout/circulant".into()),
        };
        edges.push(Edge {
            id: (i + 1) * 3,
            source: node(source),
            kind: 1 + i % 3,
            target: node(target),
        });
    }
    Ok(edges)
}
type Adjacency = BTreeMap<Node, Vec<u64>>;
fn adjacency(edges: &[Edge], incoming: bool) -> Adjacency {
    let mut out: Adjacency = BTreeMap::new();
    for edge in edges {
        out.entry(if incoming { edge.target } else { edge.source })
            .or_default()
            .push(edge.id);
    }
    for ids in out.values_mut() {
        ids.sort_unstable();
    }
    out
}
fn put_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 128 {
        out.push((n as u8 & 0x7f) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}
fn put_edge(out: &mut Vec<u8>, e: Edge) {
    for n in [
        e.id,
        e.source.namespace,
        e.source.id,
        e.kind,
        e.target.namespace,
        e.target.id,
    ] {
        put_u64(out, n);
    }
}
fn validate_edges(edges: &[Edge]) -> Result<()> {
    if edges.is_empty() || edges.len() > MAX_EDGES {
        return Err("invalid edge count".into());
    }
    let mut previous = 0;
    for e in edges {
        if e.id <= previous || e.source.id == 0 || e.target.id == 0 || e.kind == 0 {
            return Err("invalid or unordered canonical edge".into());
        }
        previous = e.id;
    }
    Ok(())
}
fn encode_section(layout: Layout, edges: &[Edge], incoming: bool) -> Vec<u8> {
    let canonical: BTreeMap<_, _> = edges.iter().map(|e| (e.id, *e)).collect();
    let buckets = adjacency(edges, incoming);
    let mut out = Vec::new();
    let variable = matches!(layout, Layout::SortedVarint | Layout::Delta);
    if variable {
        put_varint(&mut out, buckets.len() as u64);
    } else {
        put_u64(&mut out, buckets.len() as u64);
    }
    let mut previous = Node {
        namespace: 0,
        id: 0,
    };
    for (key, ids) in buckets {
        if variable {
            if layout == Layout::Delta {
                put_varint(&mut out, key.namespace - previous.namespace);
                put_varint(
                    &mut out,
                    if key.namespace == previous.namespace {
                        key.id - previous.id
                    } else {
                        key.id
                    },
                );
            } else {
                put_varint(&mut out, key.namespace);
                put_varint(&mut out, key.id);
            }
            put_varint(&mut out, ids.len() as u64);
        } else {
            put_u64(&mut out, key.namespace);
            put_u64(&mut out, key.id);
            put_u64(&mut out, ids.len() as u64);
        }
        let mut previous_id = 0u64;
        for id in ids {
            match layout {
                Layout::Duplicated => put_edge(&mut out, canonical[&id]),
                Layout::References => put_u64(&mut out, id),
                Layout::SortedVarint => put_varint(&mut out, id),
                Layout::Delta => put_varint(&mut out, id - previous_id),
            }
            previous_id = id;
        }
        previous = key;
    }
    out
}
fn encode(layout: Layout, edges: &[Edge]) -> Result<Vec<u8>> {
    validate_edges(edges)?;
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&layout.number().to_le_bytes());
    out.extend_from_slice(&(edges.len() as u32).to_le_bytes());
    for e in edges {
        put_edge(&mut out, *e);
    }
    for incoming in [false, true] {
        let section = encode_section(layout, edges, incoming);
        put_u64(&mut out, section.len() as u64);
        out.extend(section);
    }
    let crc = crc32fast::hash(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    if out.len() > MAX_BYTES {
        return Err("experimental file limit exceeded".into());
    }
    Ok(out)
}
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.position.checked_add(n).ok_or("length overflow")?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or("truncated experimental file")?;
        self.position = end;
        Ok(bytes)
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn varint(&mut self) -> Result<u64> {
        let mut n = 0;
        for i in 0..10 {
            let b = self.take(1)?[0];
            if i == 9 && b > 1 {
                return Err("varint overflow".into());
            }
            n |= ((b & 0x7f) as u64) << (i * 7);
            if b & 0x80 == 0 {
                if i > 0 && b == 0 {
                    return Err("noncanonical varint".into());
                }
                return Ok(n);
            }
        }
        Err("unterminated varint".into())
    }
    fn edge(&mut self) -> Result<Edge> {
        Ok(Edge {
            id: self.u64()?,
            source: Node {
                namespace: self.u64()?,
                id: self.u64()?,
            },
            kind: self.u64()?,
            target: Node {
                namespace: self.u64()?,
                id: self.u64()?,
            },
        })
    }
    fn done(&self) -> bool {
        self.position == self.bytes.len()
    }
}
#[derive(Debug)]
struct Graph {
    edges: Vec<Edge>,
    by_id: BTreeMap<u64, usize>,
    outgoing: Adjacency,
    incoming: Adjacency,
}
impl Graph {
    fn walk(&self, start: Node, depth: usize, incoming: bool) -> Vec<u64> {
        let index = if incoming {
            &self.incoming
        } else {
            &self.outgoing
        };
        let mut queue = VecDeque::from([(start, 0)]);
        let mut visited = BTreeSet::from([start]);
        let mut found = BTreeSet::new();
        while let Some((node, level)) = queue.pop_front() {
            if level >= depth {
                continue;
            }
            if let Some(ids) = index.get(&node) {
                for id in ids {
                    let e = self.edges[self.by_id[id]];
                    found.insert(*id);
                    let next = if incoming { e.source } else { e.target };
                    if visited.insert(next) {
                        queue.push_back((next, level + 1));
                    }
                }
            }
        }
        found.into_iter().collect()
    }
}
fn decode_section(
    layout: Layout,
    bytes: &[u8],
    edges: &[Edge],
    incoming: bool,
) -> Result<Adjacency> {
    let canonical: BTreeMap<_, _> = edges.iter().map(|e| (e.id, *e)).collect();
    let mut cursor = Cursor::new(bytes);
    let variable = matches!(layout, Layout::SortedVarint | Layout::Delta);
    let count = if variable {
        cursor.varint()?
    } else {
        cursor.u64()?
    };
    if count > edges.len() as u64 {
        return Err("bucket count exceeds edge count".into());
    }
    let mut previous = Node {
        namespace: 0,
        id: 0,
    };
    let mut out = Adjacency::new();
    let mut seen = BTreeSet::new();
    for bucket in 0..count {
        let (namespace, id, size) = if variable {
            let n = cursor.varint()?;
            let i = cursor.varint()?;
            let size = cursor.varint()?;
            if layout == Layout::Delta {
                let namespace = previous
                    .namespace
                    .checked_add(n)
                    .ok_or("namespace overflow")?;
                let id = if namespace == previous.namespace {
                    previous.id.checked_add(i).ok_or("node id overflow")?
                } else {
                    i
                };
                (namespace, id, size)
            } else {
                (n, i, size)
            }
        } else {
            (cursor.u64()?, cursor.u64()?, cursor.u64()?)
        };
        let key = Node { namespace, id };
        if id == 0 || (bucket > 0 && key <= previous) || size == 0 || size > edges.len() as u64 {
            return Err("invalid adjacency bucket".into());
        }
        let mut ids = Vec::new();
        let mut previous_id = 0u64;
        for _ in 0..size {
            let id = match layout {
                Layout::Duplicated => {
                    let edge = cursor.edge()?;
                    if canonical.get(&edge.id) != Some(&edge) {
                        return Err("duplicate payload differs from canonical edge".into());
                    }
                    edge.id
                }
                Layout::References => cursor.u64()?,
                Layout::SortedVarint => cursor.varint()?,
                Layout::Delta => previous_id
                    .checked_add(cursor.varint()?)
                    .ok_or("edge id overflow")?,
            };
            let e = canonical
                .get(&id)
                .ok_or("adjacency points outside canonical table")?;
            if id <= previous_id
                || !seen.insert(id)
                || (if incoming { e.target } else { e.source }) != key
            {
                return Err("invalid adjacency membership/order".into());
            }
            ids.push(id);
            previous_id = id;
        }
        out.insert(key, ids);
        previous = key;
    }
    if !cursor.done() || seen.len() != edges.len() {
        return Err("adjacency incomplete or has trailing bytes".into());
    }
    Ok(out)
}
fn decode(bytes: &[u8]) -> Result<Graph> {
    if bytes.len() < 36 || bytes.len() > MAX_BYTES {
        return Err("invalid experimental file size".into());
    }
    let crc_offset = bytes.len() - 4;
    if crc32fast::hash(&bytes[..crc_offset]) != u32::from_le_bytes(bytes[crc_offset..].try_into()?)
    {
        return Err("experimental checksum mismatch".into());
    }
    let mut cursor = Cursor::new(&bytes[..crc_offset]);
    if cursor.take(8)? != MAGIC {
        return Err("experimental magic mismatch".into());
    }
    let layout = Layout::from_number(u32::from_le_bytes(cursor.take(4)?.try_into()?))?;
    let count = u32::from_le_bytes(cursor.take(4)?.try_into()?) as usize;
    if count == 0
        || count > MAX_EDGES
        || count.checked_mul(48).ok_or("count overflow")? > bytes.len() - 36
    {
        return Err("invalid canonical count".into());
    }
    let mut edges = Vec::with_capacity(count);
    for _ in 0..count {
        edges.push(cursor.edge()?);
    }
    validate_edges(&edges)?;
    let mut sections = Vec::new();
    for incoming in [false, true] {
        let len = usize::try_from(cursor.u64()?)?;
        if len > MAX_BYTES {
            return Err("section limit".into());
        }
        sections.push(decode_section(layout, cursor.take(len)?, &edges, incoming)?);
    }
    if !cursor.done() {
        return Err("trailing file bytes".into());
    }
    let by_id = edges.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
    Ok(Graph {
        edges,
        by_id,
        outgoing: sections.remove(0),
        incoming: sections.remove(0),
    })
}
fn oracle(edges: &[Edge], start: Node, depth: usize, incoming: bool) -> Vec<u64> {
    let mut frontier = BTreeSet::from([start]);
    let mut visited = frontier.clone();
    let mut found = BTreeSet::new();
    for _ in 0..depth {
        let mut next = BTreeSet::new();
        for e in edges {
            let (from, to) = if incoming {
                (e.target, e.source)
            } else {
                (e.source, e.target)
            };
            if frontier.contains(&from) {
                found.insert(e.id);
                if visited.insert(to) {
                    next.insert(to);
                }
            }
        }
        frontier = next;
        if frontier.is_empty() {
            break;
        }
    }
    found.into_iter().collect()
}
#[derive(Serialize)]
struct Distribution {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    total_ms: f64,
}
fn distribution(values: &[Duration]) -> Distribution {
    let mut times: Vec<_> = values.iter().map(|v| v.as_secs_f64() * 1e6).collect();
    times.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        times[((times.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(times.len() - 1)]
    };
    Distribution {
        samples: times.len(),
        p50_us: percentile(0.5),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
        total_ms: times.iter().sum::<f64>() / 1000.0,
    }
}
#[cfg(windows)]
fn process_metrics() -> Result<(f64, u64, u64)> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::{GetCurrentProcess, GetProcessTimes},
        },
    };
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    memory.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: valid current process pseudo-handle and correctly sized initialized output buffers.
    unsafe {
        let h = GetCurrentProcess();
        let [creation, exit, kernel, user] = &mut times;
        if GetProcessTimes(h, creation, exit, kernel, user) == 0
            || GetProcessMemoryInfo(h, &mut memory, memory.cb) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let ticks = |v: FILETIME| ((v.dwHighDateTime as u64) << 32) | v.dwLowDateTime as u64;
    Ok((
        (ticks(times[2]) + ticks(times[3])) as f64 / 10000.0,
        memory.WorkingSetSize as u64,
        memory.PeakWorkingSetSize as u64,
    ))
}
#[cfg(not(windows))]
fn process_metrics() -> Result<(f64, u64, u64)> {
    Err("Windows CPU/RAM counters required; no invented substitutes".into())
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 6 {
        return Err("usage: index-layout NEW_DIRECTORY PROFILE LAYOUT EDGE_COUNT SAMPLES".into());
    }
    let path = Path::new(&args[1]);
    let profile = &args[2];
    let layout = Layout::parse(&args[3])?;
    let count: usize = args[4].parse()?;
    let samples: usize = args[5].parse()?;
    if samples == 0 || samples > 1000 {
        return Err("samples must be 1..1000".into());
    }
    let original = make_edges(profile, count)?;
    let (cpu_start, _, _) = process_metrics()?;
    let start = Instant::now();
    let bytes = encode(layout, &original)?;
    let encode_ms = start.elapsed().as_secs_f64() * 1000.0;
    fs::create_dir(path)?;
    let file = path.join("graph.idxexp");
    let start = Instant::now();
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    drop(output);
    let write_sync_ms = start.elapsed().as_secs_f64() * 1000.0;
    let file_hash = format!("{:x}", Sha256::digest(&bytes));
    drop(bytes);
    let start = Instant::now();
    let restored = fs::read(&file)?;
    let graph = decode(&restored)?;
    let recovery_ms = start.elapsed().as_secs_f64() * 1000.0;
    if graph.edges != original {
        return Err("canonical roundtrip differs".into());
    }
    let canonical_sha256 = format!("{:x}", Sha256::digest(serde_json::to_vec(&original)?));
    let max_node = original
        .iter()
        .flat_map(|e| [e.source.id, e.target.id])
        .max()
        .ok_or("empty graph")?;
    let mut outgoing = Vec::new();
    let mut incoming = Vec::new();
    let mut result_hash = Sha256::new();
    for sample in 0..samples {
        let id = if sample == 0 {
            1
        } else {
            1 + (sample as u64 * 7919) % max_node
        };
        for direction in [false, true] {
            let expected = oracle(&original, node(id), 3, direction);
            let start = Instant::now();
            let found = graph.walk(node(id), 3, direction);
            let elapsed = start.elapsed();
            if found != expected {
                return Err("three-level traversal differs from independent scan oracle".into());
            }
            result_hash.update(serde_json::to_vec(&(id, direction, &found))?);
            if direction {
                incoming.push(elapsed);
            } else {
                outgoing.push(elapsed);
            }
        }
    }
    let canonical_bytes = count as u64 * 48;
    let total_bytes = fs::metadata(&file)?.len();
    let mut cursor = Cursor::new(&restored);
    cursor.take(16 + count * 48)?;
    let outgoing_bytes = cursor.u64()?;
    cursor.take(outgoing_bytes as usize)?;
    let incoming_bytes = cursor.u64()?;
    let (cpu_end, working_set_bytes, peak_working_set_bytes) = process_metrics()?;
    let report = serde_json::json!({"profile":profile,"layout":layout,"edges":count,"samples_per_direction":samples,"depth":3,"canonical_bytes":canonical_bytes,"outgoing_bytes":outgoing_bytes,"incoming_bytes":incoming_bytes,"framing_bytes":36,"total_file_bytes":total_bytes,"file":file,"file_sha256":file_hash,"canonical_sha256":canonical_sha256,"traversal_sha256":format!("{:x}",result_hash.finalize()),"encode_ms":encode_ms,"write_sync_ms":write_sync_ms,"recovery_ms":recovery_ms,"outgoing_walk":distribution(&outgoing),"incoming_walk":distribution(&incoming),"cpu_ms":cpu_end-cpu_start,"working_set_bytes":working_set_bytes,"peak_working_set_bytes":peak_working_set_bytes,"verified_roundtrip":true,"verified_scan_oracle":true,"measurement_scope":"Fresh process. Canonical data held identical, both directions included; decode reconstructs same RAM graph in every layout. RAM/CPU include original dataset, serialized input, reconstructed graph, scan oracle and verification. Traversal operates on decoded RAM, not compressed in-place."});
    if canonical_bytes + outgoing_bytes + incoming_bytes + 36 != total_bytes {
        return Err("byte accounting mismatch".into());
    }
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_layouts_roundtrip_namespaces_and_three_level_walks() {
        let original = make_edges("fanout", 200).unwrap();
        for layout in Layout::ALL {
            let bytes = encode(layout, &original).unwrap();
            let decoded = decode(&bytes).unwrap();
            assert_eq!(decoded.edges, original);
            for incoming in [false, true] {
                for id in [1, 3, 12, 70] {
                    assert_eq!(
                        decoded.walk(node(id), 3, incoming),
                        oracle(&original, node(id), 3, incoming)
                    );
                }
            }
        }
    }
    #[test]
    fn corruption_truncation_duplicates_and_invalid_varints_are_rejected() {
        let original = make_edges("chain", 5).unwrap();
        for layout in Layout::ALL {
            let bytes = encode(layout, &original).unwrap();
            for cut in 0..bytes.len() {
                assert!(decode(&bytes[..cut]).is_err());
            }
            for offset in 0..bytes.len() {
                let mut bad = bytes.clone();
                bad[offset] ^= 1;
                assert!(decode(&bad).is_err());
            }
        }
        assert!(Cursor::new(&[0x80; 10]).varint().is_err());
        assert!(Cursor::new(&[0x81, 0]).varint().is_err());
        let mut duplicate = original.clone();
        duplicate.push(original[0]);
        assert!(encode(Layout::Delta, &duplicate).is_err());
    }
    #[test]
    fn reference_layouts_count_canonical_data_and_both_indices() {
        let original = make_edges("circulant", 200).unwrap();
        let full = encode(Layout::Duplicated, &original).unwrap();
        let fixed = encode(Layout::References, &original).unwrap();
        let delta = encode(Layout::Delta, &original).unwrap();
        assert!(fixed.len() < full.len());
        assert!(delta.len() < fixed.len());
        assert_eq!(decode(&full).unwrap().edges.len(), 200);
    }
}
