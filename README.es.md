# Canonical Contract Database

[English](README.md) | [Português](README.pt-BR.md) | [Español](README.es.md)

Motor embebido en Rust, multimodelo y orientado a contratos de datos, para consultas deterministas de estados, historiales y relaciones. **CCDB** es una identidad propuesta. No necesita IA ni integración con Foundry.

**API 0.1 experimental.** Sin certificación en producción. Los descriptores de contrato no ejecutan reglas de negocio; la aplicación impone el comportamiento.

## Inicio rápido

Instale Rust/Cargo compatible con edición 2024. MSRV exacto aún no cualificado. Dependencias no incluidas: la primera compilación puede necesitar acceso al registry si no hay caché o mirror. Desde la raíz:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run ./demo-db ./demo-evidence
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review ./demo-db ./demo-evidence
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- ./demo-db ./demo-client
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

Los directorios de salida deben ser nuevos antes del primer run. Review reabre y verifica el flujo; el cliente externo usa APIs públicas. Consulte la [guía completa](docs/es/guia.md): arquitectura, contratos, diez historias, operación, compatibilidad, contexto de rendimiento y límites.

## Límites

Registry confía en el proceso propietario local. Query/Graph aplican grants; Reader no tiene ACL propia. Ref no es clave foránea universal. Las transacciones no confirman archivos externos atómicamente. Las pruebas de fallos simulados no certifican cortes eléctricos. Sin servidor, búsqueda vectorial, inferencia ni integración Foundry.

## Estado

La aceptación standalone E26 pasó anteriormente 304 pruebas estándar y 315 all-features. Son resultados históricos, no promesas para otros entornos. E27 registra nuevas comprobaciones por separado. API Rust experimental, sin promesa de ABI estable.

[Contribuir](CONTRIBUTING.es.md) · [Seguridad](SECURITY.es.md)

## Licencias

El código propio está disponible bajo [MIT](LICENSE-MIT) o [Apache-2.0](LICENSE-APACHE), a elección. Mantenedor: Arthur Gomes de Freitas Cardozo (`cardozoarthur`). Las dependencias conservan sus licencias y no se atribuyen al proyecto. Consulte la [compatibilidad de licencias](THIRD_PARTY.md). Este repositorio distribuye fuentes propias, sin dependencias vendorizadas ni binarios compilados.
