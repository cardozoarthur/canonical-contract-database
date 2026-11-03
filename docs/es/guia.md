# Canonical Contract Database — guía

[English](../en/guide.md) · [Português](../guia-publico.md) · Español

Motor de base de datos embebido en Rust, multimodelo y orientado a contratos, para consultas deterministas de estados, historiales y relaciones. **Canonical Contract Database (CCDB)** es la identidad propuesta del proyecto; los crates conservan el prefijo `canonical-`.

El motor funciona sin IA. Una aplicación puede utilizarlo para memoria operativa, trazabilidad y dependencias estructuradas. No ofrece inferencia semántica, búsqueda vectorial, servidor de red ni integración con Foundry. La API Rust **0.1 es experimental**, sin promesa de ABI estable.

## Empezar desde el código fuente

Instale Git y un toolchain Rust estable reciente con Cargo y soporte para la edición Rust 2024. El proyecto aún no ha calificado una versión mínima de Rust ni todos los sistemas operativos. Utilice los archivos `Cargo.lock` publicados. Las dependencias no están vendorizadas: la resolución inicial puede necesitar acceso al registro Cargo; la base de datos en ejecución es embebida.

Ejecute desde la raíz del clon. Este ejemplo PowerShell crea rutas nuevas:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked

$ccdbDemo = Join-Path ([System.IO.Path]::GetTempPath()) ('ccdb-demo-' + [Guid]::NewGuid().ToString('N'))
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdbDemo/db" "$ccdbDemo/external-export"
```

El mismo flujo en un shell POSIX:

```sh
cargo test --manifest-path engine/Cargo.toml --workspace --locked
ccdb_demo="$(mktemp -d)"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdb_demo/db" "$ccdb_demo/external-export"
```

Los comandos POSIX describen la misma interfaz de argumentos; su presencia no certifica una ejecución en Linux/macOS. Los resultados publicados deben identificar el entorno realmente utilizado. Compruebe cada código de salida antes de continuar. No utilice rutas con datos reales: `run` exige directorios de base de datos y evidencias nuevos; el cliente externo exige una carpeta de exportación nueva.

`run` crea objetivos, tareas, contratos, requisitos y evidencias sintéticos, realiza un cambio transaccional y reabre la base para verificarla. `review` reabre y comprueba registros y archivos. El CLI también verifica rechazos de entradas inválidas, conflictos y redefinición de schema; esos intentos no publican cambios de dominio. El cliente externo consulta cuatro tareas públicas, verifica historial y grafo, exporta JSON/CSV/CBR y reabre la base. Su salida incluye `client-report.json`.

Hay un escritor cooperativo por directorio. Ejecute los procesos secuencialmente, después de finalizar el anterior. Resuelva dependencias antes de añadir `--offline`: la caché Cargo es un requisito de ese modo, no una garantía de distribución.

## Diez historias de usuario ejecutables

El [escenario de referencia](../../engine/crates/views/examples/contract_workflow.rs) y su [modelo de aplicación](../../engine/crates/views/examples/support/contract_workflow.rs) realizan estas historias. Agente, Worker y crítico son consumidores del ejemplo, sin conexión a un producto externo.

| Historia | Resultado observado | Límite del contrato |
|---|---|---|
| US-01: consultar estado actual | Tarea102 reabre activa/revisión2; la revisión1 estaba planificada | Estados y transiciones pertenecen al dominio de la aplicación |
| US-02: filtrar tareas por estado/objetivo | Objetivo1001: planificada101, activa102, completada103;104 pertenece a otro objetivo y105 está restringida | Query recibe sujeto y namespaces autorizados |
| US-03: registrar cambios por contrato | Una transacción de nueve miembros vincula acción, contrato, tarea y evidencia; una revisión obsoleta no publica una acción parcial | `Concept.contract` es descriptor opaco, no ejecutor universal de contratos |
| US-04: recuperar pruebas y requisitos | Requirement501→Test601→Artifact701→Action401, comparando contenido canónico y externo | Un valor `passed` almacenado no prueba por sí solo ejecución ni autenticidad |
| US-05: definir schemas propios | El cliente registra Resource v1/v2 en runtime; rechaza redefinición incompatible | Utiliza tipos existentes; no carga código arbitrario de plugins |
| US-06: recorrer dependencias | Camino101→102→103, relaciones tipadas y límites de recorrido | El grafo es una proyección derivada con coste de RAM |
| US-07: solicitar JSON/CSV/CBR | Exportaciones comprobadas y CBR convertido de nuevo en Records | CSV usa un schema; CBR necesita definiciones del Registry para interpretar payloads |
| US-08: reiniciar y recuperar | Reapertura verifica estado; error inyectado después de sync prueba recuperación atómica y retry idempotente | No certifica corte físico de energía ni replicación |
| US-09: conservar versiones de schema | Resource901 permanece v1 y902 usa v2; ambos reabren legibles | Una versión nueva no migra todos los registros automáticamente |
| US-10: integrar mediante API Rust pública | [Cliente en workspace independiente](../../examples/rust-client/src/main.rs) utiliza Registry, Query, Relations y Views públicos | Dependencias `path` y API experimental; no implica publicación en crates.io |

El ejemplo ejecuta cuatro comprobaciones antes del commit: bytes de evidencia, transición prevista, revisión esperada y objetivo. El verificador posterior compara archivo externo y bytes canónicos con una fixture independiente. Las [pruebas de aceptación](../../engine/crates/views/tests/contract_workflow_e26.rs) rechazan adulteración corta, adulteración del mismo tamaño y CSV corrupto. La [prueba contextual de recuperación](../../engine/crates/views/tests/contract_recovery_e26.rs) comprueba confirmación perdida tras sync, objeto envenenado, reapertura y retry idempotente.

El archivo externo y el commit no forman una transacción distribuida. La aplicación prepara el archivo y verifica su consistencia; el banco conserva sus propios registros canónicos. No hay firmas digitales ni protección frente a cualquier escritor malicioso.

## Arquitectura y mapa de APIs

El modelo canónico es la fuente lógica de verdad. El journal almacena cambios confirmados; índices y proyecciones especializados son derivados y reconstruibles. Existen referencias, cachés y estructuras RAM: «canónico» no significa ausencia de copias físicas en todas las capas.

| Crate | Responsabilidad y punto de entrada |
|---|---|
| [`canonical-core`](../../engine/crates/core/src/lib.rs) | Errores públicos y límite físico de frame |
| [`canonical-codec`](../../engine/crates/codec/src/lib.rs) | Envelope binario, serialización y validación de frames |
| [`canonical-storage`](../../engine/crates/storage/src/lib.rs) | Archivos, journal append-only, sync, exclusión cooperativa y recuperación |
| [`canonical-index`](../../engine/crates/index/src/lib.rs) | Índice primario derivado y formatos de caché |
| [`canonical-registry`](../../engine/crates/registry/src/lib.rs) | Namespaces, conceptos, schemas, registros, revisiones, transacciones, snapshots y mantenimiento |
| [`canonical-query`](../../engine/crates/query/src/lib.rs) | Filtros tipados, proyecciones, orden, paginación y sujeto autorizado |
| [`canonical-relations`](../../engine/crates/relations/src/lib.rs) | Relaciones tipadas y recorridos de grafo limitados |
| [`canonical-views`](../../engine/crates/views/src/lib.rs) | Readers JSON, CSV y CBR sobre registros suministrados por el llamador |
| [`canonical-memory`](../../engine/crates/memory/src/lib.rs) | Instrumentación opcional de asignaciones; no cambia el contrato de datos |

`canonical-bench` y `canonical-functional-bench` son workloads de medición, no servicios necesarios para la aplicación. SQLite y codecs de los comparadores no obligan al núcleo a depender de SQL para funcionar.

Genere documentación de API con:

```sh
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

La salida predeterminada está en `engine/target/doc/`; abra, por ejemplo, `canonical_registry/index.html`. `CARGO_TARGET_DIR`, si está configurado, cambia esa ubicación.

### Modelo, versiones y concurrencia

Una identidad es `(namespace, record_id)`. `SchemaRef` identifica `(namespace, id, version)`; schemas publicados son inmutables. Una estructura modificada recibe una versión nueva. Cada registro conserva su referencia de schema y tiene una revisión independiente de la versión del schema.

`Registry` registra schemas y valida valores tipados; `commit` aplica cambios con revisión esperada. Conserve el ID transaccional de 16 bytes y la petición canónica en el llamador. Un retry idéntico recupera el recibo; el mismo ID con contenido u orden diferente produce conflicto. Hay como máximo 1024 miembros por transacción y debe caber en el frame físico de 16 MiB; la preparación puede rechazar antes por su margen conservador de envelope.

Un `Ref` valida estructura y namespace; por sí solo no obliga a que exista el destino como clave foránea universal. Las relaciones nativas registradas tienen reglas tipadas y validación de endpoints. Las referencias no hacen del motor un ejecutor semántico de reglas de negocio.

Los snapshots son prefijos inmutables. La implementación comparte estructuras RAM y copia en la primera escritura si lectores conservan el mapa anterior. Esto puede aumentar RAM en workloads de escritura con lectores vivos. El bloqueo de archivos es cooperativo, sin protección frente a procesos que lo ignoren deliberadamente. No existe protocolo de lectura multiproceso concurrente con el escritor ni frontera de aislamiento de red.

### Consulta, autorización y salida

`QueryEngine` consulta snapshots mediante predicados tipados: igualdad, intervalos y combinaciones booleanas. Ofrece proyecciones de IDs, identidades, campos y registros. Un cursor pertenece a su contexto de consulta/snapshot y no concede permisos. Consulte límites y validaciones de API en lugar de asumir páginas ilimitadas.

`Subject` limita Query y Graph. `Registry` es acceso local confiado al proceso propietario. `Reader` no tiene ACL propia: reciba únicamente resultados de consulta/grafo ya autorizados. Una aplicación con API de red debe aportar sus controles en vez de exponer un Registry irrestricto.

Los readers producen representaciones en la lectura, sin requerir una base separada por formato. CSV exige un schema y política explícita de valores compuestos (`Reject` o `JsonCell`); no es importador genérico sin pérdida. CBR transporta Records con SchemaRef y payload; la interpretación tipada requiere el schema del Registry. Los límites de registros y bytes de salida son configurables. Un stream puede fallar después de emitir bytes: descarte la salida incompleta, sin tratarla como exportación válida.

### Errores para el integrador

El enum público está en `canonical-core`. `Conflict` abarca identidades/revisiones o peticiones transaccionales incompatibles; `UnknownSchema` e `InvalidInput` indican entrada inválida. No convierta `Unauthorized` en consulta irrestricta. `InvalidCursor` exige reiniciar paginación válida. `HistoryPruned` distingue payload eliminado por retención explícita. `WriterBusy` exige cerrar el otro escritor, no quitar forzosamente el lock. `Io`/`Poisoned`, `IncompleteTail`, `Corruption` y `UnsupportedVersion` requieren los procedimientos siguientes. Trate `TooLarge` como límite de petición sin desactivar validaciones.

## Operación, recuperación y backup

1. **Apertura estricta.** `Registry::open` valida el estado persistido. No sustituya un directorio existente tras un error ni ignore versiones desconocidas. El journal es autoritativo; caché/checkpoint no justifican ignorar corrupción.
2. **Resuelva confirmación indeterminada.** Un error de protocolo puede suceder después de persistir. Cierre el objeto envenenado, reabra y consulte `transaction_receipt(tx_id)` antes de repetir exactamente la petición. No reutilice el objeto para inferir éxito desde caché.
3. **Recupere cuando corresponda.** `Registry::recover` puede descartar explícitamente una cola incompleta después de conservar copia del segmento. Lea `RecoveryReport`, incluidos bytes descartados y ruta del backup. Checksum inválido en frame completo, corrupción intermedia y versión desconocida no son colas descartables. Conserve archivos para diagnóstico.
4. **Backup con banco cerrado.** Cierre todos los handles del escritor y copie el directorio completo a un destino nuevo. Guarde versión del software y hashes. Abra la copia y compruebe schemas/estado/historial/recibos; pruebe escritura sólo en una copia desechable. No copie durante escritura o compactación concurrente.
5. **Restaure en otra ruta.** Conserve el original y valide la copia con software compatible. No cambie manualmente el manifiesto a una generación anterior tras commits nuevos: ocultaría cambios confirmados. No trunque corrupción completa manualmente.

`sync_all` participa en la confirmación, pero depende del sistema de archivos y hardware. Las pruebas cubren interrupción de procesos y errores/corrupción inyectados; no corte de energía, fallo físico de disco ni replicación distribuida. Windows conserva la limitación de flush portátil de directorios de la biblioteca estándar. Una carpeta sincronizada en la nube no es protocolo de replicación del banco.

### Retención y compactación

`Registry::compact(Retention::KeepAll)` es la opción conservadora predeterminada. `KeepLast(n)` exige `n>0` y puede podar payloads históricos elegibles, conservando también revisiones protegidas por referencias/snapshots, tombstones y metadatos. Puede retener más de `n` revisiones. Un `KeepAll` posterior no recupera bytes ya podados.

`history` devuelve revisiones con payload disponible y puede tener huecos. Use `revision_status` o `lookup` para distinguir disponible, podada y ausente. `snapshot_at` falla en vez de devolver estado histórico parcial si falta una revisión necesaria por poda. Un snapshot ya emitido sigue siendo legible.

La compactación publica una generación nueva mediante protocolo recuperable. No elimina generaciones antiguas automáticamente; `cleanup_obsolete_generations` es mantenimiento explícito de alcance validado. Segmentos antiguos no sustituyen un backup completo con manifiesto. Antes de podar/limpiar, conserve y valide un backup cerrado. Compactación aún puede tener costes importantes de RAM y lectura; no hay presupuesto global fijo de memoria.

## Compatibilidad y madurez

La API Rust es experimental. Fije un commit conocido en los consumidores y ejecute pruebas de integración al actualizar. El cliente externo demuestra dependencias `path` locales, no distribución en registro público.

El lector actual conserva fixtures de formatos anteriores. El journal usa envelopes versionados, límites y checksums; cambios autoritativos requieren lectores que los entiendan. Compactación utiliza manifiestos de generación y eventos que lectores antiguos pueden no admitir. No presuponga downgrade físico: conserve una copia compatible antes de actualizar, úsela para rollback y reconcilie explícitamente commits posteriores. La caché de índice derivada puede reconstruirse; su codificación no es el wire autoritativo del journal.

CRC detecta corrupción accidental, no adulteración autenticada. Un hash transaccional identifica la petición; no es firma ni autorización. El proyecto no afirma calificación de producción, disponibilidad distribuida, migración automática de todos los schemas ni seguridad multitenant sólo por ser embebido.

## Benchmarks: resultados históricos y límites

Estas cifras resumen la comparación local de cierre **E15→E24**, del 3 de noviembre de 2026. El corpus tenía 100 mil registros principales, 11 revisiones configuradas y relaciones adicionales: seis pares AB/BA, CPU0 y 128 muestras por operación/proceso. Los perfiles de compilación entregados cambiaron durante el desarrollo; se comparan configuraciones sin atribuir cada mejora a una única causa.

| Medida | E15 | E24 |
|---|---:|---:|
| Pico de RAM del proceso, macro/System |860,47 MB |281,44 MB |
| Consulta de IDs, p50 |2.472,85 µs |222,45 µs |
| Grafo de salida, profundidad 3, p50 |9,40 µs |5,20 µs |
| Diez revisiones físicas anteriores, p50 |270,80 µs |244,75 µs |
| Reapertura |2.579,36 ms |1.722,18 ms |
| Preparación: apertura + proyecciones |2.883,98 ms |1.900,86 ms |

No es una medición nueva de esta publicación ni comparación pareada actual con SQLite/MessagePack/Protobuf. No demuestra superioridad general ni SLO en otro hardware. RAM corresponde a un perfil de lectura; lectores que retienen snapshots durante escrituras pueden aumentar el coste copy-on-write. Afinidad no garantiza CPU exclusiva, almacenamiento frío ni frecuencia fija.

Límites conservados: IDs por debajo de 200 µs no fueron estables; reapertura inferior a 1 s no se alcanzó; el p99 histórico empeoró de 555 a 576,10 µs. En la aceptación E24, registros completos empeoraron7,22% agregado frente a E23 en tres de seis pares, aceptado por la regla predefinida que también exigía al menos cuatro pares adversos para bloquear. Los costes del primer acceso al historial público y compactación no se volvieron a medir en esa comparación. Un experimento E25 fue rechazado y revertido; sus mejores cifras no describen la versión publicada.

E26 añadió ejemplos y pruebas de aceptación sin nuevas promesas de latencia/RAM. Sus resultados locales históricos fueron 304 pruebas estándar y 315 instrumentadas aprobadas. Para el commit público, consulte resultados asociados a ese commit; no convierta las cifras históricas en ejecución nueva.

Para medir su aplicación, use el mismo corpus y contenido devuelto en los comparadores. Informe perfil de compilación, hardware/SO, procesos/muestras, cachés, garantías de recuperación y coste de índices. Separe ejecución funcional equivalente de caminos optimizados específicos de cada motor. Incluya archivos auxiliares relevantes al sumar almacenamiento; mida también memoria, arranque y costes diferidos, no sólo bytes de payload.

## Verificación y contribuciones técnicas

Desde la raíz del proyecto:

```sh
cargo fmt --manifest-path engine/Cargo.toml --all -- --check
cargo clippy --manifest-path engine/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo test --manifest-path engine/Cargo.toml --workspace --all-features --locked
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

El cliente externo tiene workspace separado. Compruébelo con `cargo fmt --manifest-path examples/rust-client/Cargo.toml -- --check` y `cargo clippy --manifest-path examples/rust-client/Cargo.toml --all-targets --locked -- -D warnings`, además de ejecutar el flujo completo anterior.

Los cambios deben conservar fixtures, errores, límites de entrada, revisión esperada, idempotencia y recuperación. Un reporte de fallo debe incluir versión/commit, entorno, comando y reproducción mínima con datos sintéticos; elimine credenciales y datos privados. Para reportes sensibles, siga la política de seguridad publicada en el repositorio, sin exponer información confidencial en una issue pública.
