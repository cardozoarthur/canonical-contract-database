//! Controlled local benchmark. Codec append logs are harnesses, not database products.
use canonical_registry::{
    Field, FieldType, Level, Mutation, Namespace, NewRecord, Record, Registry, Schema, SchemaRef,
    Value,
};
use prost::Message;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File, OpenOptions},
    hint::black_box,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const BATCH: usize = 256;
const SCHEMA: SchemaRef = SchemaRef {
    namespace: 1,
    id: 1,
    version: 1,
};
const COLUMNS: [&str; 11] = [
    "id",
    "revision",
    "action",
    "file",
    "objective",
    "task",
    "worker",
    "time_ms",
    "state",
    "text",
    "success",
];

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Fact {
    id: u64,
    revision: u64,
    action: u64,
    file: u64,
    objective: u64,
    task: u64,
    worker: u64,
    time_ms: u64,
    state: u32,
    text: String,
    success: bool,
}

#[derive(Clone, PartialEq, Message)]
struct ProtoFact {
    #[prost(uint64, tag = "1")]
    id: u64,
    #[prost(uint64, tag = "2")]
    revision: u64,
    #[prost(uint64, tag = "3")]
    action: u64,
    #[prost(uint64, tag = "4")]
    file: u64,
    #[prost(uint64, tag = "5")]
    objective: u64,
    #[prost(uint64, tag = "6")]
    task: u64,
    #[prost(uint64, tag = "7")]
    worker: u64,
    #[prost(uint64, tag = "8")]
    time_ms: u64,
    #[prost(uint32, tag = "9")]
    state: u32,
    #[prost(string, tag = "10")]
    text: String,
    #[prost(bool, tag = "11")]
    success: bool,
}
#[derive(Clone, PartialEq, Message)]
struct ProtoBatch {
    #[prost(message, repeated, tag = "1")]
    rows: Vec<ProtoFact>,
}
impl From<&Fact> for ProtoFact {
    fn from(f: &Fact) -> Self {
        Self {
            id: f.id,
            revision: f.revision,
            action: f.action,
            file: f.file,
            objective: f.objective,
            task: f.task,
            worker: f.worker,
            time_ms: f.time_ms,
            state: f.state,
            text: f.text.clone(),
            success: f.success,
        }
    }
}
impl From<ProtoFact> for Fact {
    fn from(f: ProtoFact) -> Self {
        Self {
            id: f.id,
            revision: f.revision,
            action: f.action,
            file: f.file,
            objective: f.objective,
            task: f.task,
            worker: f.worker,
            time_ms: f.time_ms,
            state: f.state,
            text: f.text,
            success: f.success,
        }
    }
}
fn fact(id: u64, revision: u64) -> Fact {
    Fact {
        id,
        revision,
        action: 1 + id % 7,
        file: 1 + id % 113,
        objective: 1 + id % 17,
        task: 1 + id % 53,
        worker: 1 + id % 5,
        time_ms: 1_800_000_000_000 + id * 100 + revision,
        state: (revision % 4) as u32,
        text: format!(
            "Evidence for task {id:06}, revision {revision}: deterministic local operation; referência UTF-8."
        ),
        success: revision % 2 == 1,
    }
}
fn dataset(rows: u64, revisions: u64) -> Vec<Fact> {
    (1..=revisions)
        .flat_map(|r| (1..=rows).map(move |id| fact(id, r)))
        .collect()
}
fn input(f: &Fact) -> NewRecord {
    NewRecord {
        namespace: 1,
        id: f.id,
        schema: SCHEMA,
        values: vec![
            Some(Value::UInt(f.action)),
            Some(Value::UInt(f.file)),
            Some(Value::UInt(f.objective)),
            Some(Value::UInt(f.task)),
            Some(Value::UInt(f.worker)),
            Some(Value::UInt(f.time_ms)),
            Some(Value::UInt(f.state as u64)),
            Some(Value::Utf8(f.text.clone())),
            Some(Value::Bool(f.success)),
        ],
    }
}
fn schema() -> Schema {
    Schema {
        reference: SCHEMA,
        fields: [
            "action",
            "file",
            "objective",
            "task",
            "worker",
            "time_ms",
            "state",
            "text",
            "success",
        ]
        .iter()
        .enumerate()
        .map(|(i, label)| Field {
            label: (*label).into(),
            kind: if i == 7 {
                FieldType::Utf8
            } else if i == 8 {
                FieldType::Bool
            } else {
                FieldType::UInt
            },
            optional: false,
        })
        .collect(),
    }
}
fn from_record(db: &Registry, r: &Record) -> Result<Fact> {
    let v = db.decode_values(r.schema, &r.payload)?;
    let uint = |i: usize| -> Result<u64> {
        match v.get(i) {
            Some(Some(Value::UInt(x))) => Ok(*x),
            _ => Err("invalid integer".into()),
        }
    };
    Ok(Fact {
        id: r.id,
        revision: r.revision,
        action: uint(0)?,
        file: uint(1)?,
        objective: uint(2)?,
        task: uint(3)?,
        worker: uint(4)?,
        time_ms: uint(5)?,
        state: uint(6)?.try_into()?,
        text: match &v[7] {
            Some(Value::Utf8(t)) => t.clone(),
            _ => return Err("invalid text".into()),
        },
        success: match &v[8] {
            Some(Value::Bool(b)) => *b,
            _ => return Err("invalid boolean".into()),
        },
    })
}
fn hash(facts: &[Fact]) -> Result<String> {
    let mut hasher = Sha256::new();
    for f in facts {
        hasher.update(serde_json::to_vec(f)?);
        hasher.update([b'\n']);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
#[derive(Serialize)]
struct Distribution {
    samples: usize,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
    total_ms: f64,
}
fn distribution(samples: &[Duration]) -> Distribution {
    let mut v: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1e6).collect();
    v.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        if v.is_empty() {
            0.0
        } else {
            v[((v.len() as f64 * p).ceil() as usize)
                .saturating_sub(1)
                .min(v.len() - 1)]
        }
    };
    Distribution {
        samples: v.len(),
        p50_us: percentile(0.50),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
        total_ms: v.iter().sum::<f64>() / 1000.0,
    }
}
#[derive(Serialize)]
struct Metrics {
    backend: String,
    rows: u64,
    revisions: u64,
    history_records: usize,
    batch_records: usize,
    logical_sha256: String,
    payload_bytes: u64,
    total_disk_bytes: u64,
    files: BTreeMap<String, u64>,
    serialize: Distribution,
    write_batch: Distribution,
    read_revision: Distribution,
    write_records_per_second: f64,
    read_records_per_second: f64,
    recovery_ms: f64,
    index_flush_ms: f64,
    cpu_ms: f64,
    working_set_bytes: u64,
    peak_working_set_bytes: u64,
    measurement_scope: String,
    notes: Vec<String>,
}
struct Observations {
    serialize: Vec<Duration>,
    write: Vec<Duration>,
    read: Vec<Duration>,
    payload: u64,
    recovery: Duration,
    flush: Duration,
    recovered: Vec<Fact>,
}
impl Observations {
    fn new() -> Self {
        Self {
            serialize: vec![],
            write: vec![],
            read: vec![],
            payload: 0,
            recovery: Duration::ZERO,
            flush: Duration::ZERO,
            recovered: vec![],
        }
    }
}
fn timed<T>(samples: &mut Vec<Duration>, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let start = Instant::now();
    let result = f()?;
    samples.push(start.elapsed());
    Ok(result)
}
fn run_engine(path: &Path, all: &[Fact], rows: u64) -> Result<Observations> {
    let mut o = Observations::new();
    let mut db = Registry::open(path)?;
    db.register_namespace(Namespace {
        id: 1,
        name: "benchmark".into(),
        level: Level::Local,
    })?;
    db.register_schema(schema())?;
    for f in all {
        o.payload += timed(&mut o.serialize, || {
            let n = input(f);
            let r = Record {
                namespace: 1,
                id: f.id,
                schema: SCHEMA,
                revision: f.revision,
                payload: db.encode_values(SCHEMA, &n.values)?,
            };
            Ok(black_box(canonical_codec::encode(&r)?).len() as u64)
        })?;
    }
    let mut transaction = 1u64;
    // Split at revision boundary as well as batch boundary, preserving identical commits.
    for version in all.chunks(rows as usize) {
        for batch in version.chunks(BATCH) {
            let mut id = [0u8; 16];
            id[..8].copy_from_slice(&transaction.to_le_bytes());
            transaction += 1;
            timed(&mut o.write, || {
                if batch[0].revision == 1 {
                    db.create_batch(id, &batch.iter().map(input).collect::<Vec<_>>())?;
                } else {
                    db.commit(
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
                Ok(())
            })?;
        }
    }
    let start = Instant::now();
    db.flush_index()?;
    o.flush = start.elapsed();
    drop(db);
    let start = Instant::now();
    let mut db = Registry::open(path)?;
    o.recovery = start.elapsed();
    for expected in all {
        let r = timed(&mut o.read, || {
            let found = db
                .lookup(1, expected.id, Some(expected.revision))?
                .revision
                .ok_or("missing revision")?;
            from_record(&db, &found.record)
        })?;
        o.recovered.push(r);
    }
    if db.snapshot().len() != rows as usize
        || (1..=rows).map(|id| db.history(1, id).len()).sum::<usize>() != all.len()
    {
        return Err("engine identity/history counts differ".into());
    }
    for expected in &all[all.len() - rows as usize..] {
        let latest = db
            .lookup(1, expected.id, None)?
            .revision
            .ok_or("missing head")?;
        if from_record(&db, &latest.record)? != *expected {
            return Err("engine latest differs".into());
        }
    }
    Ok(o)
}
fn encode_fact(backend: &str, f: &Fact) -> Result<Vec<u8>> {
    Ok(match backend {
        "json" => serde_json::to_vec(f)?,
        "messagepack" => rmp_serde::to_vec(f)?,
        "protobuf" => ProtoFact::from(f).encode_to_vec(),
        _ => return Err("unknown codec".into()),
    })
}
fn encode_batch(backend: &str, batch: &[Fact]) -> Result<Vec<u8>> {
    Ok(match backend {
        "json" => serde_json::to_vec(batch)?,
        "messagepack" => rmp_serde::to_vec(batch)?,
        "protobuf" => ProtoBatch {
            rows: batch.iter().map(ProtoFact::from).collect(),
        }
        .encode_to_vec(),
        _ => return Err("unknown codec".into()),
    })
}
fn decode_batch(backend: &str, bytes: &[u8]) -> Result<Vec<Fact>> {
    Ok(match backend {
        "json" => serde_json::from_slice(bytes)?,
        "messagepack" => rmp_serde::from_slice(bytes)?,
        "protobuf" => ProtoBatch::decode(bytes)?
            .rows
            .into_iter()
            .map(Fact::from)
            .collect(),
        _ => return Err("unknown codec".into()),
    })
}
fn run_log(backend: &str, path: &Path, all: &[Fact], rows: u64) -> Result<Observations> {
    let mut o = Observations::new();
    fs::create_dir_all(path)?;
    fs::write(
        path.join("schema.json"),
        serde_json::to_vec(
            &serde_json::json!({"columns":COLUMNS,"uint_fields":"u64 except state u32","text":"utf8","success":"bool","order":"revision then id","framing":"u32 length, batch payload, crc32 little endian","version":1}),
        )?,
    )?;
    OpenOptions::new()
        .write(true)
        .open(path.join("schema.json"))?
        .sync_all()?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path.join("history.log"))?;
    for f in all {
        o.payload += timed(&mut o.serialize, || {
            Ok(black_box(encode_fact(backend, f)?).len() as u64)
        })?;
    }
    for version in all.chunks(rows as usize) {
        for batch in version.chunks(BATCH) {
            timed(&mut o.write, || {
                let bytes = encode_batch(backend, batch)?;
                file.write_all(&u32::try_from(bytes.len())?.to_le_bytes())?;
                file.write_all(&bytes)?;
                file.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;
                file.sync_all()?;
                Ok(())
            })?;
        }
    }
    drop(file);
    let start = Instant::now();
    let mut file = File::open(path.join("history.log"))?;
    let mut index = BTreeMap::new();
    loop {
        let mut length = [0u8; 4];
        let got = file.read(&mut length)?;
        if got == 0 {
            break;
        }
        if got != 4 {
            return Err("truncated frame".into());
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > 16 * 1024 * 1024 {
            return Err("oversize frame".into());
        }
        let mut bytes = vec![0u8; length];
        file.read_exact(&mut bytes)?;
        let mut crc = [0; 4];
        file.read_exact(&mut crc)?;
        if crc32fast::hash(&bytes) != u32::from_le_bytes(crc) {
            return Err("corrupt frame".into());
        }
        for f in decode_batch(backend, &bytes)? {
            if index.insert((f.id, f.revision), f).is_some() {
                return Err("duplicate revision".into());
            }
        }
    }
    o.recovery = start.elapsed();
    if index.len() != all.len() {
        return Err("log history count differs".into());
    }
    for f in all {
        o.recovered.push(timed(&mut o.read, || {
            Ok(index
                .get(&(f.id, f.revision))
                .ok_or("missing revision")?
                .clone())
        })?);
    }
    for expected in &all[all.len() - rows as usize..] {
        if index
            .range((expected.id, 0)..=(expected.id, u64::MAX))
            .next_back()
            .map(|(_, f)| f)
            != Some(expected)
        {
            return Err("log latest differs".into());
        }
    }
    Ok(o)
}
fn run_sqlite(path: &Path, all: &[Fact], rows: u64) -> Result<Observations> {
    let mut o = Observations::new();
    fs::create_dir_all(path)?;
    let p = path.join("history.sqlite");
    let mut db = Connection::open(&p)?;
    // DELETE/FULL synchronizes each complete transaction; no retained uncounted WAL.
    db.pragma_update(None, "journal_mode", "DELETE")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.execute_batch("CREATE TABLE facts(id INTEGER NOT NULL,revision INTEGER NOT NULL,action INTEGER NOT NULL,file INTEGER NOT NULL,objective INTEGER NOT NULL,task INTEGER NOT NULL,worker INTEGER NOT NULL,time_ms INTEGER NOT NULL,state INTEGER NOT NULL,text TEXT NOT NULL,success INTEGER NOT NULL,PRIMARY KEY(id,revision)) WITHOUT ROWID;")?;
    // SQLite has no external wire serialization: measure binding/materializing logical values,
    // and report payload as logical UTF-8 + fixed integer widths, not SQLite on-disk encoding.
    for f in all {
        o.payload += timed(&mut o.serialize, || {
            let values = (
                f.id,
                f.revision,
                f.action,
                f.file,
                f.objective,
                f.task,
                f.worker,
                f.time_ms,
                f.state,
                black_box(f.text.as_bytes()),
                f.success,
            );
            black_box(values);
            Ok(8 * 8 + 4 + 1 + f.text.len() as u64)
        })?;
    }
    for version in all.chunks(rows as usize) {
        for batch in version.chunks(BATCH) {
            timed(&mut o.write, || {
                let tx = db.transaction()?;
                {
                    let mut stmt =
                        tx.prepare_cached("INSERT INTO facts VALUES(?,?,?,?,?,?,?,?,?,?,?)")?;
                    for f in batch {
                        stmt.execute(params![
                            f.id,
                            f.revision,
                            f.action,
                            f.file,
                            f.objective,
                            f.task,
                            f.worker,
                            f.time_ms,
                            f.state,
                            f.text,
                            f.success
                        ])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })?;
        }
    }
    db.close().map_err(|(_, e)| e)?;
    let start = Instant::now();
    let db = Connection::open(&p)?;
    // Force b-tree access; connection-open alone would undercount deferred recovery.
    let count: u64 = db.query_row("SELECT COUNT(*) FROM facts", [], |r| r.get(0))?;
    if count != all.len() as u64 {
        return Err("SQLite history count mismatch".into());
    }
    o.recovery = start.elapsed();
    let mut stmt=db.prepare_cached("SELECT id,revision,action,file,objective,task,worker,time_ms,state,text,success FROM facts WHERE id=? AND revision=?")?;
    for f in all {
        o.recovered.push(timed(&mut o.read, || {
            Ok(stmt.query_row(params![f.id, f.revision], |r| {
                Ok(Fact {
                    id: r.get(0)?,
                    revision: r.get(1)?,
                    action: r.get(2)?,
                    file: r.get(3)?,
                    objective: r.get(4)?,
                    task: r.get(5)?,
                    worker: r.get(6)?,
                    time_ms: r.get(7)?,
                    state: r.get(8)?,
                    text: r.get(9)?,
                    success: r.get(10)?,
                })
            })?)
        })?);
    }
    let mut latest=db.prepare_cached("SELECT id,revision,action,file,objective,task,worker,time_ms,state,text,success FROM facts WHERE id=? ORDER BY revision DESC LIMIT 1")?;
    for expected in &all[all.len() - rows as usize..] {
        let f = latest.query_row([expected.id], |r| {
            Ok(Fact {
                id: r.get(0)?,
                revision: r.get(1)?,
                action: r.get(2)?,
                file: r.get(3)?,
                objective: r.get(4)?,
                task: r.get(5)?,
                worker: r.get(6)?,
                time_ms: r.get(7)?,
                state: r.get(8)?,
                text: r.get(9)?,
                success: r.get(10)?,
            })
        })?;
        if f != *expected {
            return Err("SQLite latest differs".into());
        }
    }
    Ok(o)
}
fn disk(path: &Path) -> Result<BTreeMap<String, u64>> {
    fn walk(root: &Path, path: &Path, out: &mut BTreeMap<String, u64>) -> Result<()> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                walk(root, &entry.path(), out)?;
            } else if meta.is_file() {
                out.insert(
                    entry
                        .path()
                        .strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                    meta.len(),
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(path, path, &mut files)?;
    Ok(files)
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
    // SAFETY: pseudo-handle is valid for this process, pointers refer to live correctly sized structs.
    unsafe {
        let h = GetCurrentProcess();
        let [creation, exit, kernel, user] = &mut times;
        if GetProcessTimes(h, creation, exit, kernel, user) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if GetProcessMemoryInfo(h, &mut memory, memory.cb) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    let ticks = |t: &FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    Ok((
        (ticks(&times[2]) + ticks(&times[3])) as f64 / 10_000.0,
        memory.WorkingSetSize as u64,
        memory.PeakWorkingSetSize as u64,
    ))
}
#[cfg(not(windows))]
fn process_metrics() -> Result<(f64, u64, u64)> {
    Err("This benchmark requires Windows process counters; do not substitute fabricated CPU/RAM values".into())
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        return Err("usage: canonical-bench BACKEND NEW_DIRECTORY ROWS REVISIONS".into());
    }
    let backend = &args[1];
    let path = PathBuf::from(&args[2]);
    if path.exists() {
        return Err("benchmark directory must not exist".into());
    }
    let rows: u64 = args[3].parse()?;
    let revisions: u64 = args[4].parse()?;
    if rows == 0 || revisions == 0 || rows > 100_000 || revisions > 20 {
        return Err("rows 1..100000 and revisions 1..20 required".into());
    }
    let all = dataset(rows, revisions);
    let (cpu_start, _, _) = process_metrics()?;
    let o = match backend.as_str() {
        "engine" => run_engine(&path, &all, rows)?,
        "json" | "messagepack" | "protobuf" => run_log(backend, &path, &all, rows)?,
        "sqlite" => run_sqlite(&path, &all, rows)?,
        _ => return Err("backend must be engine/json/messagepack/protobuf/sqlite".into()),
    };
    if all != o.recovered {
        return Err("logical history equivalence check failed".into());
    }
    let logical_sha256 = hash(&o.recovered)?;
    let (cpu_end, working_set_bytes, peak_working_set_bytes) = process_metrics()?;
    let files = disk(&path)?;
    let write = distribution(&o.write);
    let read = distribution(&o.read);
    let result=Metrics{backend:backend.clone(),rows,revisions,history_records:all.len(),batch_records:BATCH,logical_sha256,payload_bytes:o.payload,total_disk_bytes:files.values().sum(),files,serialize:distribution(&o.serialize),write_records_per_second:all.len() as f64/(write.total_ms/1000.0),read_records_per_second:all.len() as f64/(read.total_ms/1000.0),write_batch:write,read_revision:read,recovery_ms:o.recovery.as_secs_f64()*1000.0,index_flush_ms:o.flush.as_secs_f64()*1000.0,cpu_ms:cpu_end-cpu_start,working_set_bytes,peak_working_set_bytes,measurement_scope:"one fresh OS process; dataset, codec, database, verification and allocator included; CPU measured from after dataset creation through full verification; RAM is process working set/peak".into(),notes:vec!["Warm OS page cache; reopening is measured, not cold device recovery or power failure".into(),"One synthetic run is descriptive; repeat and inspect variability before optimization decisions".into(),"Every version retained; no deletions/relations/retention in this dataset".into(),"No tokens measurement: no agreed task or equivalent tokenizer".into()]};
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
