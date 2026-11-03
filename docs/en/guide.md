# Canonical Contract Database — guide

English · [Português](../guia-publico.md) · [Español](../es/guia.md)

An embedded, contract-oriented, multimodel database engine in Rust for deterministic state, history and relationship queries. **Canonical Contract Database (CCDB)** is the proposed project identity; the Rust crates retain the `canonical-` prefix.

The engine works without AI. Applications can use it for operational memory, traceability and structured dependencies. It does not provide semantic inference, vector search, a network server or Foundry integration. The Rust **0.1 API is experimental**, with no stable ABI promise.

## Start from source

Install Git and a recent stable Rust toolchain with Cargo and Rust 2024 edition support. The project has not qualified a minimum supported Rust version or all operating systems. Use the published `Cargo.lock` files. Dependencies are not vendored: initial resolution may need access to the Cargo registry; the running database is embedded.

Run these commands at the clone root. This PowerShell example creates fresh paths:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked

$ccdbDemo = Join-Path ([System.IO.Path]::GetTempPath()) ('ccdb-demo-' + [Guid]::NewGuid().ToString('N'))
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdbDemo/db" "$ccdbDemo/external-export"
```

The same workflow in a POSIX shell:

```sh
cargo test --manifest-path engine/Cargo.toml --workspace --locked
ccdb_demo="$(mktemp -d)"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdb_demo/db" "$ccdb_demo/external-export"
```

The POSIX commands describe the same argument interface; their presence does not certify a Linux/macOS run. Published validation results must identify the environment actually used. Check each exit code before continuing. Do not use a path containing real data: `run` requires new database and evidence directories, and the external client requires a new export directory.

`run` creates synthetic objectives, tasks, contracts, requirements and evidence, makes a transactional change, then reopens and verifies the database. `review` reopens that database and checks records and files. The CLI also tests rejection of invalid inputs, conflicts and schema redefinition; these attempts do not publish domain changes. The external client queries four public tasks, checks history and graph, exports JSON/CSV/CBR, and reopens the database. Its output includes `client-report.json`.

The database allows one cooperative writer per directory. Run the processes sequentially, after the previous process has exited. Resolve dependencies before adding `--offline`: the Cargo cache is a prerequisite for offline builds, not a distribution guarantee.

## Ten executable user stories

The [reference scenario](../../engine/crates/views/examples/contract_workflow.rs) and its [application model](../../engine/crates/views/examples/support/contract_workflow.rs) implement these stories. Roles such as agent, Worker and critic are example consumers with no connection to an external product.

| Story | Observed scenario result | Contract boundary |
|---|---|---|
| US-01: query current state | Task102 reopens active at revision2; revision1 was planned | States and transitions belong to the application domain |
| US-02: filter tasks by state/objective | Objective1001: planned101, active102, completed103;104 belongs elsewhere and105 is restricted | Query receives an authorized subject and namespaces |
| US-03: record contract-linked changes | One transaction has nine members linking action, contract, task and evidence; a stale revision cannot publish a partial action | `Concept.contract` is opaque metadata, not a universal contract executor |
| US-04: retrieve tests and requirements | Requirement501→Test601→Artifact701→Action401, with canonical and external content compared | A stored `passed` value alone proves neither execution nor authenticity |
| US-05: define custom schemas | The client registers Resource v1/v2 at runtime; conflicting redefinition is rejected | Uses existing field types; does not load arbitrary plugin code |
| US-06: traverse dependencies | Path101→102→103 with typed relations and traversal limits | Graph is a derived projection whose RAM costs must be included |
| US-07: request JSON/CSV/CBR | Exports are checked and CBR converts back to Records | CSV uses one schema; interpreting typed CBR payloads requires Registry definitions |
| US-08: restart and recover | Reopening verifies state; an injected error after sync tests atomic recovery and idempotent retry | Does not certify physical power loss or replication |
| US-09: preserve schema versions | Resource901 stays v1 and902 uses v2; both reopen readable | A new version does not migrate all records automatically |
| US-10: integrate through public Rust APIs | A [separate-workspace client](../../examples/rust-client/src/main.rs) uses public Registry, Query, Relations and Views | Local `path` dependencies and experimental APIs; no crates.io publication implied |

The example executes four checks before committing: evidence bytes, expected transition, expected revision and objective. The later verifier compares the external file and canonical bytes against an independent fixture. [Acceptance tests](../../engine/crates/views/tests/contract_workflow_e26.rs) reject short tampering, same-length tampering and corrupted CSV. The [contextual recovery test](../../engine/crates/views/tests/contract_recovery_e26.rs) checks lost acknowledgement after sync, a poisoned object, reopening and idempotent retry.

The external file and database commit are not a distributed transaction. The application prepares the file and checks consistency; the database stores its own canonical records. The scenario provides neither digital signatures nor protection against every malicious writer.

## Architecture and API map

The canonical model is the logical source of truth. The journal stores committed changes; specialized indexes and projections are derived and rebuildable. References, caches and RAM structures exist: “canonical” does not mean that every layer avoids physical copies.

| Crate | Responsibility and entry point |
|---|---|
| [`canonical-core`](../../engine/crates/core/src/lib.rs) | Public errors and physical frame limit |
| [`canonical-codec`](../../engine/crates/codec/src/lib.rs) | Binary envelope, serialization and frame validation |
| [`canonical-storage`](../../engine/crates/storage/src/lib.rs) | Files, append-only journal, sync, cooperative exclusion and recovery |
| [`canonical-index`](../../engine/crates/index/src/lib.rs) | Derived primary index and cache formats |
| [`canonical-registry`](../../engine/crates/registry/src/lib.rs) | Namespaces, concepts, schemas, records, revisions, transactions, snapshots and maintenance |
| [`canonical-query`](../../engine/crates/query/src/lib.rs) | Typed filters, projections, ordering, pagination and authorized subjects |
| [`canonical-relations`](../../engine/crates/relations/src/lib.rs) | Typed relations and bounded graph traversals |
| [`canonical-views`](../../engine/crates/views/src/lib.rs) | JSON, CSV and CBR readers over caller-supplied records |
| [`canonical-memory`](../../engine/crates/memory/src/lib.rs) | Optional allocation instrumentation; does not change the data contract |

`canonical-bench` and `canonical-functional-bench` are measurement workloads, not required application services. SQLite and codecs used by comparison tools do not make the core engine depend on SQL to operate.

Generate API documentation with:

```sh
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

The default output is `engine/target/doc/`; open, for example, `canonical_registry/index.html`. A configured `CARGO_TARGET_DIR` changes that location.

### Model, versions and concurrency

An identity is `(namespace, record_id)`. `SchemaRef` identifies `(namespace, id, version)`; published schemas are immutable. Give a changed structure a new version. Records preserve their schema reference and have their own revision, independent of schema version.

`Registry` registers schemas and validates typed values; `commit` applies changes with expected revisions. Keep the 16-byte transaction ID and canonical request in the caller. An identical retry recovers the receipt; the same ID with different content or ordering conflicts. A transaction has at most 1024 members and must fit the16 MiB physical frame limit; preparation may reject it earlier because of its conservative envelope allowance.

A `Ref` validates shape and namespace; it alone does not enforce target existence as a universal foreign key. Registered native relations have typed rules and endpoint validation. References do not turn the database into a semantic business-rule executor.

Snapshots represent immutable prefixes. The implementation shares RAM structures and copies on the first write while readers retain the previous map. This can increase RAM in write workloads with live readers. File locking is cooperative and does not stop a process that deliberately ignores it. There is no multiprocess reader protocol concurrent with the writer, nor a network isolation boundary.

### Query, authorization and output

`QueryEngine` queries snapshots using typed predicates such as equality, ranges and boolean combinations. It supports ID, identity, field and record projections. A cursor belongs to its corresponding query/snapshot context and grants no permission. Consult API limits and validation instead of assuming unbounded pages.

`Subject` limits Query and Graph. `Registry` is trusted local access for the owning process. `Reader` has no independent ACL: provide only already authorized query/graph results. An application exposing a network API must supply its own controls rather than exposing an unrestricted Registry reference.

Readers generate representations on demand without requiring a separate database per format. CSV requires one schema and an explicit compound-value policy (`Reject` or `JsonCell`); it is not a general lossless importer. CBR carries Records with SchemaRef and payload; typed interpretation requires the Registry schema. Record/output byte limits are configurable. Stream writes can fail after emitting bytes: discard incomplete output rather than treating it as a valid export.

### Errors an integrator must handle

The public enum is in `canonical-core`. `Conflict` covers incompatible identities, revisions or transaction requests; `UnknownSchema` and `InvalidInput` indicate invalid input. Do not turn `Unauthorized` into an unrestricted query. `InvalidCursor` requires restarting valid pagination. `HistoryPruned` distinguishes payload removed by explicit retention. `WriterBusy` requires closing the other writer, not forcibly removing the lock. `Io`/`Poisoned`, `IncompleteTail`, `Corruption` and `UnsupportedVersion` require the operating procedures below. Treat `TooLarge` as a request limit rather than disabling validation.

## Operation, recovery and backup

1. **Open strictly.** `Registry::open` validates persisted state. Do not replace an existing directory on error or ignore an unknown version. The journal is authoritative; a cache/checkpoint does not justify ignoring corruption.
2. **Resolve uncertain acknowledgement.** A protocol error may occur after persistence. Close the poisoned object, reopen, and check `transaction_receipt(tx_id)` before retrying exactly the request. Do not reuse the object to infer success from cache.
3. **Recover only when appropriate.** `Registry::recover` explicitly may discard an incomplete tail after preserving a segment copy. Read `RecoveryReport`, including discarded bytes and backup path. Invalid checksums in complete frames, intermediate corruption and unknown versions are not discardable tails. Preserve files for diagnosis.
4. **Back up closed databases.** Close every writer handle and copy the complete directory to a fresh destination. Keep software version and hashes. Open the copy and verify schemas/state/history/receipts; test writes only in a disposable copy. Do not copy during concurrent writes or compaction.
5. **Restore elsewhere.** Preserve the original and validate the copy with compatible software. Do not manually switch the manifest to an old generation after later commits: that would hide acknowledged changes. Do not manually truncate complete corruption.

`sync_all` participates in acknowledgement but depends on filesystem and hardware. Tests cover process interruption and injected corruption/errors, not power loss, physical disk failure or distributed replication. Windows retains the standard-library limitation of portable directory flushing. A cloud-synchronized directory is not a database replication protocol.

### Retention and compaction

`Registry::compact(Retention::KeepAll)` is the conservative default. `KeepLast(n)` requires `n>0` and can prune eligible historical payloads while preserving revisions protected by references/snapshots, tombstones and metadata. It may retain more than `n` revisions. Later `KeepAll` does not recover previously pruned bytes.

`history` returns revisions with available payloads and may have gaps. Use `revision_status` or `lookup` to distinguish available, pruned and missing. `snapshot_at` fails instead of returning partial historical state when a needed revision was pruned. An already-issued snapshot remains readable.

Compaction publishes a new generation through a recoverable protocol. Old generations are not deleted automatically; `cleanup_obsolete_generations` is explicit maintenance with validated scope. Old segments do not replace a complete backup with its manifest. Before pruning/cleanup, preserve and validate a closed backup. Compaction can still have substantial RAM/read costs; there is no fixed global memory budget.

## Compatibility and maturity

The Rust API is experimental. Pin a known commit in consumers and run integration tests when upgrading. The external client demonstrates local `path` dependencies, not public registry distribution.

The current reader retains older-format fixtures. The journal uses versioned envelopes, limits and checksums; authoritative format changes require readers that understand them. Compaction uses generation manifests and events older readers may not support. Do not assume physical downgrade compatibility: preserve a compatible copy before upgrading, use it to roll back, and explicitly reconcile later commits. The derived index cache can be rebuilt; its encoding is distinct from authoritative journal wire format.

CRC detects accidental corruption, not authenticated tampering. Transaction hashes identify requests; they are neither signatures nor authorization. The project does not claim production qualification, distributed availability, automatic migration of all schemas, or multitenant security merely because it is embedded.

## Benchmarks: historical results and boundaries

These numbers summarize the local **E15→E24** closing comparison on 3 November 2026. The corpus contained 100,000 primary records, 11 configured revisions and additional relations, measured over six AB/BA pairs, CPU0 and 128 samples per operation/process. Delivered build profiles changed during development, so this compares configurations without attributing every gain to a single cause.

| Measure | E15 | E24 |
|---|---:|---:|
| Process peak RAM, macro/System |860.47 MB |281.44 MB |
| ID query, p50 |2,472.85 µs |222.45 µs |
| Outgoing graph, depth 3, p50 |9.40 µs |5.20 µs |
| Ten previous physical revisions, p50 |270.80 µs |244.75 µs |
| Reopening |2,579.36 ms |1,722.18 ms |
| Readiness: open + projections |2,883.98 ms |1,900.86 ms |

This is neither a new publication measurement nor a current paired comparison against SQLite/MessagePack/Protobuf. It demonstrates neither general superiority nor an SLO on other hardware. RAM was measured in a read profile; readers retaining snapshots across writes can increase copy-on-write costs. Affinity does not guarantee an exclusive CPU, cold storage or fixed frequency.

Preserved limitations: IDs below 200 µs were not stable; reopening below 1 s was not reached; historical p99 worsened from 555 to 576.10 µs. E24 acceptance observed a 7.22% aggregate regression in full records against E23 in three of six pairs, accepted under the predefined rule that also required at least four adverse pairs to block. First public-history access and compaction costs were not remeasured in that comparison. An E25 experiment was rejected and reverted; its best numbers do not describe the published version.

E26 added examples and acceptance tests without a new latency/RAM promise. Historical local results were 304 standard and 315 instrumented tests passed. For the public commit, use validation results tied to that commit; do not treat historical counts as a new execution.

When measuring your application, use the same corpus and returned content across comparators. Report build profile, hardware/OS, process/sample counts, caches, recovery guarantees and index cost. Separate functionally equivalent execution from engine-specific optimized paths. Include relevant auxiliary files in total storage and measure memory, startup and deferred costs, not just payload bytes.

## Verification and technical contributions

At the project root:

```sh
cargo fmt --manifest-path engine/Cargo.toml --all -- --check
cargo clippy --manifest-path engine/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo test --manifest-path engine/Cargo.toml --workspace --all-features --locked
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

The external client is a separate workspace. Check it with `cargo fmt --manifest-path examples/rust-client/Cargo.toml -- --check` and `cargo clippy --manifest-path examples/rust-client/Cargo.toml --all-targets --locked -- -D warnings`, in addition to the full workflow above.

Changes must preserve fixtures, errors, input limits, expected revisions, idempotency and recovery. A bug report should include version/commit, environment, command and a minimal synthetic reproduction; remove credentials and private data. Follow the repository's published security policy for sensitive reports, without exposing confidential information in a public issue.
