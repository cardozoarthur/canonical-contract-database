# Canonical Contract Database — guia

[English](en/guide.md) · Português · [Español](es/guia.md)

Um motor de banco de dados embarcado em Rust, multimodelo e orientado a contratos, para consultas determinísticas de estados, históricos e relações. **Canonical Contract Database (CCDB)** é uma proposta de posicionamento e nome de trabalho. Os crates continuam com o prefixo `canonical-`.

O motor não precisa de IA para funcionar. Uma aplicação pode usá-lo para memória operacional, rastreabilidade e dependências estruturadas. Não oferece inferência semântica, busca vetorial, servidor de rede ou integração com o Foundry. A API Rust **0.1 é experimental**; não há promessa de ABI estável.

## Começar a partir do código-fonte

Instale Git e uma versão estável recente do toolchain Rust, com Cargo e suporte à edição Rust 2024. Este projeto ainda não qualifica uma versão mínima de Rust nem todos os sistemas operacionais. Use os arquivos `Cargo.lock` publicados. As dependências não são vendorizadas: a primeira resolução pode precisar de acesso ao registro Cargo; o banco em execução é embarcado.

Execute os comandos na raiz do clone. O exemplo abaixo usa PowerShell e cria nomes novos a cada execução:

```powershell
cargo test --manifest-path engine/Cargo.toml --workspace --locked

$ccdbDemo = Join-Path ([System.IO.Path]::GetTempPath()) ('ccdb-demo-' + [Guid]::NewGuid().ToString('N'))
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdbDemo/db" "$ccdbDemo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdbDemo/db" "$ccdbDemo/external-export"
```

Em um shell POSIX, o mesmo fluxo é:

```sh
cargo test --manifest-path engine/Cargo.toml --workspace --locked
ccdb_demo="$(mktemp -d)"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- run "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path engine/Cargo.toml -p canonical-views --example contract_workflow --locked -- review "$ccdb_demo/db" "$ccdb_demo/evidence"
cargo run --manifest-path examples/rust-client/Cargo.toml --locked -- "$ccdb_demo/db" "$ccdb_demo/external-export"
```

Esses comandos POSIX descrevem a mesma interface de argumentos; sua presença não certifica uma execução em Linux/macOS. Os resultados de validação publicados devem identificar o ambiente realmente usado. Após cada comando, confira o código de saída antes de continuar. Não reutilize um caminho com dados reais: `run` exige diretórios de banco e evidências novos, e o cliente externo exige uma pasta de exportação nova.

`run` cria objetivos, tarefas, contratos, requisitos e evidências sintéticos, faz uma mudança transacional e reabre o banco para verificá-la. `review` reabre esse banco e confere dados e arquivos. O CLI também testa rejeições de entradas inválidas, conflitos e redefinição de schema; essas tentativas não publicam alterações de domínio. O cliente externo consulta quatro tarefas públicas, verifica histórico e grafo, exporta JSON/CSV/CBR e reabre o banco. Sua saída inclui `client-report.json`.

O banco tem um escritor cooperativo por diretório. Execute os processos acima em sequência, com o processo anterior terminado. Para usar `--offline`, resolva as dependências primeiro; o cache de Cargo é uma condição do modo offline, não uma propriedade de distribuição deste projeto.

## Dez histórias de usuário executáveis

O [cenário de referência](../engine/crates/views/examples/contract_workflow.rs) e seu [modelo de aplicação](../engine/crates/views/examples/support/contract_workflow.rs) realizam as histórias abaixo. Papéis como agente, Worker e crítico são consumidores do exemplo, sem conexão a um produto externo.

| História | Resultado observado pelo cenário | Limite do contrato |
|---|---|---|
| US-01: consultar estado atual | Tarefa102 reabre ativa, na revisão2; a revisão1 era planejada | Estados e transições pertencem ao domínio da aplicação |
| US-02: filtrar tarefas por estado/objetivo | Objetivo1001: planejada101, ativa102, concluída103;104 pertence a outro objetivo e105 é restrita | Query recebe sujeito e namespaces autorizados |
| US-03: registrar alterações por contrato | Uma transação contém nove membros e relaciona ação, contrato, tarefa e evidência; revisão obsoleta não publica ação parcial | `Concept.contract` é descritor opaco, não executor universal de contratos |
| US-04: recuperar testes e requisitos | Requirement501→Test601→Artifact701→Action401, com conteúdo canônico e arquivo confrontados | Um campo `passed` isolado não prova execução nem autenticidade |
| US-05: definir schemas próprios | Cliente registra Resource v1/v2 em runtime e uma redefinição conflitante é rejeitada | Usa tipos já existentes; não carrega código arbitrário de plugin |
| US-06: navegar dependências | Caminho101→102→103, com relações tipadas e limites de travessia | O grafo é uma projeção derivada; seus custos de RAM devem ser considerados |
| US-07: solicitar JSON/CSV/CBR | Exportações conferidas e CBR convertido de volta em Records | CSV usa um schema; CBR exige Registry para interpretar payloads tipados |
| US-08: reiniciar e recuperar | Reabertura verifica estado; teste injeta erro após sync e confere recuperação atômica e retry idempotente | Não certifica corte físico de energia ou replicação |
| US-09: preservar versões antigas | Resource901 continua v1 e902 usa v2; ambos reabrem legíveis | Uma versão nova não migra registros automaticamente |
| US-10: integrar por Rust público | [Cliente em workspace separado](../examples/rust-client/src/main.rs) usa Registry, Query, Relations e Views públicos | Dependências `path`, API experimental; não é publicação no crates.io |

O exemplo executa quatro verificações antes da transação: bytes da evidência, transição prevista, revisão esperada e objetivo. O verificador posterior compara o arquivo externo e os bytes canônicos contra uma fixture independente. Os [testes de aceitação](../engine/crates/views/tests/contract_workflow_e26.rs) rejeitam adulteração curta, adulteração de tamanho idêntico e CSV corrompido. O [teste contextual de recuperação](../engine/crates/views/tests/contract_recovery_e26.rs) verifica confirmação perdida após sync, objeto envenenado, reabertura e repetição idempotente.

O arquivo externo e o commit do banco não formam uma transação distribuída. A aplicação prepara o arquivo e verifica sua consistência; o banco guarda seus próprios registros canônicos. O cenário não implementa assinatura digital nem proteção contra qualquer escritor malicioso.

## Arquitetura e mapa das APIs

O modelo canônico é a fonte lógica de verdade. O journal guarda as alterações confirmadas; índices e projeções especializadas são derivados e reconstruíveis. Há referências, caches e estruturas em RAM: “canônico” não significa ausência de cópias físicas em todas as camadas.

| Crate | Responsabilidade e ponto de entrada |
|---|---|
| [`canonical-core`](../engine/crates/core/src/lib.rs) | Erros públicos e limite físico de frame |
| [`canonical-codec`](../engine/crates/codec/src/lib.rs) | Envelope binário, serialização e validação de frames |
| [`canonical-storage`](../engine/crates/storage/src/lib.rs) | Arquivos, journal append-only, sync, exclusão cooperativa e recuperação |
| [`canonical-index`](../engine/crates/index/src/lib.rs) | Índice primário derivado e formatos de cache |
| [`canonical-registry`](../engine/crates/registry/src/lib.rs) | Namespaces, conceitos, schemas, registros, revisões, transações, snapshots e manutenção |
| [`canonical-query`](../engine/crates/query/src/lib.rs) | Filtros tipados, projeções, ordenação, paginação e sujeito autorizado |
| [`canonical-relations`](../engine/crates/relations/src/lib.rs) | Relações tipadas e travessias de grafo com limites |
| [`canonical-views`](../engine/crates/views/src/lib.rs) | Readers JSON, CSV e CBR sobre registros fornecidos pelo chamador |
| [`canonical-memory`](../engine/crates/memory/src/lib.rs) | Instrumentação opcional de alocações; não altera o contrato de dados |

As ferramentas `canonical-bench` e `canonical-functional-bench` são workloads de medição, não serviços necessários à aplicação. SQLite e codecs usados por comparadores não tornam o núcleo dependente de um banco SQL para operar.

Para consultar a documentação gerada das APIs:

```sh
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

A saída padrão começa em `engine/target/doc/`; abra, por exemplo, `canonical_registry/index.html`. Um `CARGO_TARGET_DIR` configurado muda esse destino.

### Modelo, versões e concorrência

Uma identidade é `(namespace, record_id)`. `SchemaRef` identifica `(namespace, id, version)`; schemas publicados são imutáveis. Uma nova estrutura deve receber uma nova versão. O registro preserva sua referência de schema e tem sua própria revisão, independente da versão do schema.

`Registry` registra schemas e valida valores tipados; `commit` aplica mudanças com revisão esperada. Conserve o ID transacional de 16 bytes e o pedido canônico no chamador. Retry idêntico recupera o recibo; o mesmo ID com conteúdo ou ordem diferente conflita. A unidade transacional tem até 1024 membros e deve caber no frame físico limitado a 16 MiB; a preparação pode rejeitar um lote antes do limite exato por seu envelope conservador.

Um `Ref` valida estrutura e namespace; isoladamente, não impõe existência do alvo como chave estrangeira universal. Relações nativas registradas têm regras tipadas e validação de endpoints. Referências não transformam o motor em um executor semântico das regras de negócio.

Snapshots representam prefixos imutáveis. A implementação compartilha estruturas em RAM, com cópia na primeira escrita quando leitores ainda retêm o mapa anterior. Isso pode aumentar a RAM em cargas de escrita com leitores vivos. O lock de arquivos é cooperativo: não protege contra um processo que o ignore deliberadamente. Não há protocolo de leitura multiprocesso concorrente ao escritor nem isolamento de rede.

### Consulta, autorização e saída

`QueryEngine` consulta snapshots com predicados tipados, como igualdade, intervalos e combinações booleanas. Oferece projeções de IDs, identidades, campos e registros. Um cursor pertence ao contexto de consulta/snapshot correspondente; não concede permissão. Consulte os limites e as validações nas APIs em vez de assumir páginas ilimitadas.

`Subject` limita Query e Graph. `Registry` é acesso local confiado ao processo proprietário. `Reader` não possui ACL própria: forneça apenas os resultados já autorizados da consulta/grafo. Não exponha uma referência irrestrita ao Registry em uma API de rede sem controle da aplicação.

Readers produzem representação na leitura, sem exigir um banco separado por formato. CSV exige um schema e política explícita para valores compostos (`Reject` ou `JsonCell`); não é um importador genérico sem perda. CBR transporta Records com SchemaRef e payload; o schema precisa existir no Registry para interpretação tipada. Limites de quantidade e tamanho de saída são configuráveis. Uma escrita em stream pode falhar depois de emitir bytes: descarte uma saída incompleta em vez de tratá-la como exportação válida.

### Erros que o integrador deve tratar

O enum público está em `canonical-core`. `Conflict` cobre identidades/revisões ou pedidos transacionais incompatíveis; `UnknownSchema` e `InvalidInput` indicam entradas inválidas. `Unauthorized` não deve ser convertido em uma consulta irrestrita. `InvalidCursor` exige reiniciar a paginação válida. `HistoryPruned` distingue payload removido por retenção explícita. `WriterBusy` pede encerramento do outro escritor, não remoção forçada do lock. `Io`/`Poisoned`, `IncompleteTail`, `Corruption` e `UnsupportedVersion` requerem os procedimentos de operação abaixo. `TooLarge` deve ser tratado como limite do pedido, sem desativar validações.

## Operação, recuperação e backup

1. **Abra estritamente.** `Registry::open` valida o estado persistido. Não substitua um diretório existente após erro nem ignore uma versão desconhecida. O journal é autoritativo; cache/checkpoint não autoriza ignorar corrupção.
2. **Resolva confirmação indeterminada.** Um erro durante o protocolo pode ocorrer depois da persistência. Feche o objeto envenenado, reabra e consulte `transaction_receipt(tx_id)` antes de repetir exatamente o pedido. Não reutilize o objeto para confirmar sucesso a partir de cache.
3. **Recupere somente quando apropriado.** `Registry::recover` é uma operação explícita que pode descartar uma cauda incompleta após preservar uma cópia do segmento. Leia `RecoveryReport`, incluindo bytes descartados e caminho do backup. Checksum inválido em frame completo, corrupção intermediária ou versão desconhecida não são tratados como cauda descartável. Preserve os arquivos para diagnóstico.
4. **Faça backup fechado.** Termine todos os handles do escritor e copie o diretório completo para um destino novo. Guarde a versão do software e hashes. Abra a cópia, confira schemas/estado/histórico/recibos e teste escrita somente em uma cópia descartável. Não copie em escrita ou compactação concorrente.
5. **Restaure em outro caminho.** Preserve o original; valide a cópia com uma versão compatível. Não troque manualmente o manifesto para uma geração anterior após novos commits: isso esconderia alterações confirmadas. Não trunque corrupção completa manualmente.

`sync_all` participa do protocolo de confirmação, mas depende do sistema de arquivos e hardware. Os testes cobrem interrupção de processo e corrupção/erros injetados. Não comprovam perda de energia, falha física do disco ou replicação distribuída. No Windows, permanece a limitação de flush portátil de diretórios da biblioteca padrão. Uma pasta sincronizada na nuvem não é um protocolo de replicação do banco.

### Retenção e compactação

`Registry::compact(Retention::KeepAll)` é a opção conservadora e a política padrão. `KeepLast(n)` exige `n>0` e pode podar payloads históricos elegíveis; guarda também revisões protegidas por referências/snapshots, tombstones e metadados. Pode conservar mais que `n` revisões. `KeepAll` posterior não recupera payload já podado.

`history` retorna somente revisões com payload disponível, podendo haver lacunas. Use `revision_status` ou `lookup` quando precisar distinguir disponível, podado e ausente. `snapshot_at` falha em vez de entregar estado histórico parcial quando uma revisão necessária foi podada. Um snapshot já emitido permanece legível.

Compactação publica uma nova geração por protocolo recuperável. Gerações antigas não são apagadas automaticamente; `cleanup_obsolete_generations` é uma manutenção explícita com escopo validado. Segmentos antigos não substituem um backup completo com seu manifesto. Antes de podar ou limpar, conserve e valide um backup fechado. A compactação ainda pode ter custos importantes de RAM e leitura; não há orçamento global constante de memória.

## Compatibilidade e maturidade

A API Rust é experimental. Fixe um commit conhecido no consumidor e execute seus testes de integração ao atualizar. O cliente externo deste projeto demonstra dependências locais `path`; ele não implica distribuição dos crates em um registro público.

O leitor atual conserva fixtures dos formatos anteriores. O journal usa envelopes versionados, limites e checksums; mudanças autoritativas exigem leitores que as entendam. A compactação usa manifesto de geração e eventos que leitores antigos podem não compreender. Não presuma downgrade físico: antes de atualizar conserve uma cópia compatível; para voltar use essa cópia e reconcilie commits posteriores explicitamente. Cache do índice é derivado e pode ser reconstruído; sua codificação não equivale ao wire autoritativo do journal.

CRC detecta corrupção acidental, não adulteração autenticada. Hash transacional identifica o pedido; não é assinatura nem autorização. O projeto não afirma qualificação de produção, disponibilidade distribuída, migração automática de todos os schemas ou segurança multitenant apenas por ser um banco embarcado.

## Benchmarks: resultados históricos e seus limites

Os números abaixo resumem a comparação de fechamento **E15→E24**, realizada localmente em 03/11/2026. O corpus tinha 100 mil registros principais, 11 revisões configuradas e relações adicionais; foram seis pares AB/BA, CPU0,128 amostras por operação e processo. Os perfis de compilação entregues mudaram durante a evolução, portanto a tabela compara configurações, sem atribuir todo ganho a uma causa isolada.

| Medida | E15 | E24 |
|---|---:|---:|
| Pico de RAM do processo, macro/System |860,47 MB |281,44 MB |
| Consulta de IDs, p50 |2.472,85 µs |222,45 µs |
| Grafo de saída, profundidade 3, p50 |9,40 µs |5,20 µs |
| Dez revisões físicas anteriores, p50 |270,80 µs |244,75 µs |
| Reabertura |2.579,36 ms |1.722,18 ms |
| Prontidão: abertura + projeções |2.883,98 ms |1.900,86 ms |

Não é uma medição nova da publicação, nem uma comparação pareada atual com SQLite/MessagePack/Protobuf. Não prova superioridade geral ou um SLO em outro equipamento. A medição de RAM é de um perfil de leitura; leitores retendo snapshots durante escrita podem aumentar o custo por copy-on-write. Afinidade não garante CPU exclusiva, dispositivo frio ou frequência constante.

Limites preservados no resultado histórico: IDs abaixo de 200 µs não foram estáveis; reabertura abaixo de 1 s não foi atingida; o p99 histórico piorou de 555 para 576,10 µs. Na aceitação E24, registros completos tiveram piora agregada de 7,22% frente à E23 em três de seis pares, aceita sob a regra predefinida que exigia também ao menos quatro pares adversos. Custos de primeiro acesso ao histórico público e compactação não foram reavaliados nessa comparação. Uma experiência E25 foi rejeitada e retirada; seus melhores números não descrevem a versão publicada.

E26 acrescentou exemplos e testes de aceitação, sem nova promessa de latência ou RAM. Seus resultados locais históricos foram 304 testes padrão e 315 com instrumentação aprovados. Para o commit público, use os resultados de validação identificados pelo commit correspondente; não trate as contagens históricas como uma nova execução.

Para medir sua aplicação, use o mesmo corpus e resultado retornado nos comparadores, informe perfil de compilação, hardware/SO, quantidade de processos e amostras, caches, garantias de recuperação e custo de índices. Separe execução funcional equivalente de caminhos específicos otimizados. Some todos os arquivos relevantes ao informar armazenamento total e meça também memória, inicialização e custos diferidos; não compare apenas o tamanho de payloads.

## Verificação e contribuições técnicas

Na raiz do projeto:

```sh
cargo fmt --manifest-path engine/Cargo.toml --all -- --check
cargo clippy --manifest-path engine/Cargo.toml --workspace --all-targets --locked -- -D warnings
cargo test --manifest-path engine/Cargo.toml --workspace --locked
cargo test --manifest-path engine/Cargo.toml --workspace --all-features --locked
cargo doc --manifest-path engine/Cargo.toml --workspace --no-deps --locked
```

O workspace do cliente externo é separado. Confira-o com `cargo fmt --manifest-path examples/rust-client/Cargo.toml -- --check` e `cargo clippy --manifest-path examples/rust-client/Cargo.toml --all-targets --locked -- -D warnings`, além de executar o fluxo completo acima.

Mudanças devem preservar fixtures, erros, limites de entrada, revisão esperada, idempotência e recuperação. Ao relatar uma falha, inclua versão/commit, ambiente, comando e um caso mínimo com dados sintéticos; remova credenciais e dados privados. Relatos potencialmente sensíveis devem seguir a política de segurança publicada no repositório, sem expor dados confidenciais em uma issue pública.
