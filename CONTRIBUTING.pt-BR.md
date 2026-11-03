# Contribuir

[English](CONTRIBUTING.md) | [Português](CONTRIBUTING.pt-BR.md) | [Español](CONTRIBUTING.es.md)

A API Rust 0.1 é experimental. Descreva problema, comportamento esperado e reprodução mínima. Preserve escopo, compatibilidade, recuperação e autorização. Acrescente testes comportamentais pertinentes quando alterar código.

Em engine/: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. Gere a documentação da API com `cargo doc --workspace --no-deps --locked`.

Não envie segredos ou dados privados. Contribuições devem ser fornecidas sob os termos duplos MIT/Apache-2.0 do projeto, identificando material de terceiros e suas condições originais. Não há compromisso de prazo de resposta ou merge.
