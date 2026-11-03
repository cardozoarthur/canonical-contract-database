# Contributing

[English](CONTRIBUTING.md) | [Português](CONTRIBUTING.pt-BR.md) | [Español](CONTRIBUTING.es.md)

The Rust 0.1 API is experimental. Describe the problem, expected behavior and minimal reproduction. Keep changes bounded and preserve compatibility, recovery and authorization. Add meaningful behavioral tests for code changes.

From engine/: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. Build API documentation with `cargo doc --workspace --no-deps --locked`.

Do not submit secrets or private data. Contributions must be provided under the project dual MIT/Apache-2.0 terms, with any third-party material and its original terms identified. No response time or merge commitment is promised.
