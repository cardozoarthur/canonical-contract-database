# Canonical Contract Database

[English](README.md) | [Português](README.pt-BR.md) | [Español](README.es.md)

An embedded, contract-oriented, multimodel database engine in Rust for deterministic state, history and relationship queries. **CCDB** is a proposed project identity. It needs no AI runtime or Foundry integration.

**Experimental API 0.1.** Not certified for production. Contract descriptors do not execute business rules; applications enforce behavior.

## Quickstart

Install Rust/Cargo supporting edition 2024. An exact MSRV has not been qualified. Dependencies are not vendored: the first build may require registry access unless a cache or mirror is available. From the repository root:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run ./demo-db ./demo-evidence
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review ./demo-db ./demo-evidence
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- ./demo-db ./demo-client
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

The demo output directories must not exist before the first run. Review reopens and verifies the stored workflow; the external client uses public Rust APIs. See the [complete guide](docs/en/guide.md) for architecture, contracts, all ten user stories, operation, compatibility, performance context and limitations.

## Boundaries

Registry trusts the owning local process. Query/Graph apply grants; Reader has no independent ACL. Ref is not a universal foreign key. Database transactions do not atomically commit external files. Simulated recovery tests do not certify power-loss behavior. No server, vector search, inference or Foundry integration is provided.

## Project status

The E26 standalone acceptance suite previously passed 304 default and 315 all-feature tests. These are historical results, not a promise for other environments. The E27 publication preparation records fresh checks separately. Rust APIs are experimental, with no stable Rust ABI promise.

[Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## Licensing

Project-authored code is available under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option. Maintainer: Arthur Gomes de Freitas Cardozo (`cardozoarthur`). Dependencies retain their own licenses; they are not relicensed as project-owned code. See [dependency license compatibility](THIRD_PARTY.md). This repository distributes project source, not vendored dependency source or compiled binaries.
