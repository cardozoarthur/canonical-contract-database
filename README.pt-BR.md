# Canonical Contract Database

[English](README.md) | [Português](README.pt-BR.md) | [Español](README.es.md)

Motor embarcado em Rust, multimodelo e orientado a contratos de dados, para consultas determinísticas de estados, históricos e relações. **CCDB** é uma identidade proposta. Não exige IA ou integração Foundry.

**API 0.1 experimental.** Sem certificação em produção. Descritores de contrato não executam regras de negócio; a aplicação impõe o comportamento.

## Início rápido

Instale Rust/Cargo com edição 2024. MSRV exato ainda não qualificado. Dependências não vendorizadas: a primeira compilação pode exigir acesso ao registry se não houver cache ou mirror. Na raiz do repositório:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run ./demo-db ./demo-evidence
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review ./demo-db ./demo-evidence
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- ./demo-db ./demo-client
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

Os diretórios de saída devem ser novos no primeiro run. Review reabre e verifica o fluxo; o cliente externo usa APIs públicas. Veja o [guia completo](docs/guia-publico.md): arquitetura, contratos, dez histórias, operação, compatibilidade, contexto de desempenho e limites.

## Limites

Registry confia no processo proprietário local. Query/Graph aplicam grants; Reader não tem ACL própria. Ref não é chave estrangeira universal. Transações do banco não confirmam arquivos externos atomicamente. Testes de falha simulada não certificam queda de energia. Sem servidor, busca vetorial, inferência ou integração Foundry.

## Estado

A aceitação standalone E26 passou anteriormente em 304 testes padrão e 315 all-features. São resultados históricos, não promessa para outros ambientes. A preparação E27 registra novos checks separadamente. API Rust experimental, sem promessa de ABI estável.

[Contribuir](CONTRIBUTING.pt-BR.md) · [Segurança](SECURITY.pt-BR.md)

## Licenciamento

Código próprio disponível sob [MIT](LICENSE-MIT) ou [Apache-2.0](LICENSE-APACHE), à sua escolha. Mantenedor: Arthur Gomes de Freitas Cardozo (`cardozoarthur`). Dependências mantêm suas próprias licenças e não são reclassificadas como código próprio. Veja a [compatibilidade das licenças](THIRD_PARTY.md). O repositório distribui fontes do projeto, sem fontes de dependências vendorizadas ou binários compilados.
