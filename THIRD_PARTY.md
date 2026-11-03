# Dependency license compatibility / Compatibilidade / Compatibilidad

## English

The published tree contains project-authored source and Cargo lockfiles, with no vendored dependency source or compiled binaries. Cargo obtains third-party packages separately and keeps their original license files. The project MIT/Apache-2.0 choice applies to its own code, not to ownership of dependencies.

Review of the 78 external packages pinned by the two lockfiles found no license expression requiring the whole project to be distributed under GPL/LGPL or another reciprocal license when choosing the permitted alternatives below. This is a bounded dependency review, not proof of universal originality or a certification of future dependency versions.

- For `MIT OR Apache-2.0`, choose MIT; legacy `MIT/Apache-2.0` denotes the upstream dual-license option (checked against package license texts).
- `r-efi` offers MIT OR Apache-2.0 OR LGPL-2.1-or-later: choose MIT, not the LGPL option.
- `rustix` and `linux-raw-sys` offer an MIT alternative; choose MIT.
- `unicode-ident` requires `(MIT OR Apache-2.0) AND Unicode-3.0`: choose MIT AND Unicode-3.0. The Unicode notice obligation remains; it does not require relicensing unrelated project code.
- `foldhash` is Zlib: preserve origin and required notices in any redistribution of its source.
- `prost` and `prost-derive` are Apache-2.0 only; their terms remain applicable to those components.

No dependency licenses are replaced by the project licenses. If distributing binaries or vendoring dependencies later, retain required copyright/permission notices, license texts and any applicable NOTICE files with those distributions; recheck the actual dependency graph and artifacts. No binaries or vendor bundle are offered here. SQLite is used by benchmark comparators, not as the production storage engine; SQLCipher is not enabled.

## Português

A publicação contém fontes próprias e lockfiles, sem fontes de dependências vendorizadas ou binários. Cargo obtém os pacotes separadamente, com suas licenças originais. A revisão dos 78 pacotes fixados não encontrou obrigação de relicenciar todo o projeto sob GPL/LGPL ao selecionar as alternativas permitidas: MIT para licenças duplas, r-efi e rustix/linux-raw-sys; MIT E Unicode-3.0 para unicode-ident; Zlib para foldhash; Apache-2.0 para prost/prost-derive. As condições Unicode, Zlib e Apache continuam valendo para seus componentes. Não atribuímos suas autorias ao projeto.

Se futuramente houver binários ou dependências vendorizadas, preservar textos, copyrights, permissões e NOTICE exigidos e revisar os artefatos reais. Esta análise é limitada às versões fixadas; não comprova originalidade universal. SQLite é comparador de benchmark, não o motor de armazenamento; SQLCipher não está habilitado.

## Español

La publicación contiene fuentes propias y lockfiles, sin dependencias vendorizadas ni binarios. Cargo obtiene los paquetes por separado con sus licencias originales. La revisión de los 78 paquetes fijados no encontró obligación de relicenciar todo el proyecto bajo GPL/LGPL al elegir las alternativas permitidas: MIT para licencias duales, r-efi y rustix/linux-raw-sys; MIT Y Unicode-3.0 para unicode-ident; Zlib para foldhash; Apache-2.0 para prost/prost-derive. Las condiciones Unicode, Zlib y Apache siguen aplicándose a sus componentes. No se atribuye su autoría al proyecto.

Si se distribuyen binarios o dependencias vendorizadas en el futuro, conservar textos, copyrights, permisos y NOTICE exigidos y revisar los artefactos reales. El análisis se limita a versiones fijadas; no demuestra originalidad universal. SQLite es comparador de benchmark, no motor de almacenamiento; SQLCipher no está habilitado.

## Locked packages / Pacotes fixados / Paquetes fijados

| Crate | Version | Declared license |
|---|---|---|
| [anyhow](https://crates.io/crates/anyhow/1.0.104) | 1.0.104 | `MIT OR Apache-2.0` |
| [atomic-polyfill](https://crates.io/crates/atomic-polyfill/1.0.3) | 1.0.3 | `MIT OR Apache-2.0` |
| [autocfg](https://crates.io/crates/autocfg/1.5.1) | 1.5.1 | `Apache-2.0 OR MIT` |
| [bitflags](https://crates.io/crates/bitflags/2.13.2) | 2.13.2 | `MIT OR Apache-2.0` |
| [block-buffer](https://crates.io/crates/block-buffer/0.10.4) | 0.10.4 | `MIT OR Apache-2.0` |
| [byteorder](https://crates.io/crates/byteorder/1.5.0) | 1.5.0 | `Unlicense OR MIT` |
| [bytes](https://crates.io/crates/bytes/1.12.1) | 1.12.1 | `MIT` |
| [cc](https://crates.io/crates/cc/1.11.0) | 1.11.0 | `MIT OR Apache-2.0` |
| [cfg-if](https://crates.io/crates/cfg-if/1.0.5) | 1.0.5 | `MIT OR Apache-2.0` |
| [cobs](https://crates.io/crates/cobs/0.3.0) | 0.3.0 | `MIT OR Apache-2.0` |
| [cpufeatures](https://crates.io/crates/cpufeatures/0.2.17) | 0.2.17 | `MIT OR Apache-2.0` |
| [crc32fast](https://crates.io/crates/crc32fast/1.5.4) | 1.5.4 | `MIT OR Apache-2.0` |
| [critical-section](https://crates.io/crates/critical-section/1.2.0) | 1.2.0 | `MIT OR Apache-2.0` |
| [crypto-common](https://crates.io/crates/crypto-common/0.1.7) | 0.1.7 | `MIT OR Apache-2.0` |
| [digest](https://crates.io/crates/digest/0.10.7) | 0.10.7 | `MIT OR Apache-2.0` |
| [either](https://crates.io/crates/either/1.19.0) | 1.19.0 | `MIT OR Apache-2.0` |
| [embedded-io](https://crates.io/crates/embedded-io/0.4.0) | 0.4.0 | `MIT OR Apache-2.0` |
| [embedded-io](https://crates.io/crates/embedded-io/0.6.1) | 0.6.1 | `MIT OR Apache-2.0` |
| [errno](https://crates.io/crates/errno/0.3.14) | 0.3.14 | `MIT OR Apache-2.0` |
| [fallible-iterator](https://crates.io/crates/fallible-iterator/0.3.0) | 0.3.0 | `MIT/Apache-2.0` |
| [fallible-streaming-iterator](https://crates.io/crates/fallible-streaming-iterator/0.1.9) | 0.1.9 | `MIT/Apache-2.0` |
| [fastrand](https://crates.io/crates/fastrand/2.5.0) | 2.5.0 | `Apache-2.0 OR MIT` |
| [find-msvc-tools](https://crates.io/crates/find-msvc-tools/0.1.15) | 0.1.15 | `MIT OR Apache-2.0` |
| [foldhash](https://crates.io/crates/foldhash/0.1.5) | 0.1.5 | `Zlib` |
| [fs2](https://crates.io/crates/fs2/0.4.3) | 0.4.3 | `MIT/Apache-2.0` |
| [generic-array](https://crates.io/crates/generic-array/0.14.7) | 0.14.7 | `MIT` |
| [getrandom](https://crates.io/crates/getrandom/0.4.3) | 0.4.3 | `MIT OR Apache-2.0` |
| [hash32](https://crates.io/crates/hash32/0.2.1) | 0.2.1 | `MIT OR Apache-2.0` |
| [hashbrown](https://crates.io/crates/hashbrown/0.15.5) | 0.15.5 | `MIT OR Apache-2.0` |
| [hashlink](https://crates.io/crates/hashlink/0.10.0) | 0.10.0 | `MIT OR Apache-2.0` |
| [heapless](https://crates.io/crates/heapless/0.7.17) | 0.7.17 | `MIT OR Apache-2.0` |
| [itertools](https://crates.io/crates/itertools/0.14.0) | 0.14.0 | `MIT OR Apache-2.0` |
| [itoa](https://crates.io/crates/itoa/1.0.18) | 1.0.18 | `MIT OR Apache-2.0` |
| [libc](https://crates.io/crates/libc/0.2.191) | 0.2.191 | `MIT OR Apache-2.0` |
| [libsqlite3-sys](https://crates.io/crates/libsqlite3-sys/0.35.0) | 0.35.0 | `MIT` |
| [linux-raw-sys](https://crates.io/crates/linux-raw-sys/0.12.1) | 0.12.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| [lock_api](https://crates.io/crates/lock_api/0.4.14) | 0.4.14 | `MIT OR Apache-2.0` |
| [memchr](https://crates.io/crates/memchr/2.8.3) | 2.8.3 | `Unlicense OR MIT` |
| [num-traits](https://crates.io/crates/num-traits/0.2.19) | 0.2.19 | `MIT OR Apache-2.0` |
| [once_cell](https://crates.io/crates/once_cell/1.21.4) | 1.21.4 | `MIT OR Apache-2.0` |
| [pkg-config](https://crates.io/crates/pkg-config/0.3.34) | 0.3.34 | `MIT OR Apache-2.0` |
| [postcard](https://crates.io/crates/postcard/1.1.3) | 1.1.3 | `MIT OR Apache-2.0` |
| [proc-macro2](https://crates.io/crates/proc-macro2/1.0.107) | 1.0.107 | `MIT OR Apache-2.0` |
| [prost](https://crates.io/crates/prost/0.14.4) | 0.14.4 | `Apache-2.0` |
| [prost-derive](https://crates.io/crates/prost-derive/0.14.4) | 0.14.4 | `Apache-2.0` |
| [quote](https://crates.io/crates/quote/1.0.47) | 1.0.47 | `MIT OR Apache-2.0` |
| [r-efi](https://crates.io/crates/r-efi/6.0.0) | 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` |
| [rmp](https://crates.io/crates/rmp/0.8.15) | 0.8.15 | `MIT` |
| [rmp-serde](https://crates.io/crates/rmp-serde/1.3.1) | 1.3.1 | `MIT` |
| [rusqlite](https://crates.io/crates/rusqlite/0.37.0) | 0.37.0 | `MIT` |
| [rustc_version](https://crates.io/crates/rustc_version/0.4.1) | 0.4.1 | `MIT OR Apache-2.0` |
| [rustix](https://crates.io/crates/rustix/1.1.6) | 1.1.6 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| [scopeguard](https://crates.io/crates/scopeguard/1.2.0) | 1.2.0 | `MIT OR Apache-2.0` |
| [semver](https://crates.io/crates/semver/1.0.28) | 1.0.28 | `MIT OR Apache-2.0` |
| [serde](https://crates.io/crates/serde/1.0.229) | 1.0.229 | `MIT OR Apache-2.0` |
| [serde_core](https://crates.io/crates/serde_core/1.0.229) | 1.0.229 | `MIT OR Apache-2.0` |
| [serde_derive](https://crates.io/crates/serde_derive/1.0.229) | 1.0.229 | `MIT OR Apache-2.0` |
| [serde_json](https://crates.io/crates/serde_json/1.0.154) | 1.0.154 | `MIT OR Apache-2.0` |
| [sha2](https://crates.io/crates/sha2/0.10.9) | 0.10.9 | `MIT OR Apache-2.0` |
| [shlex](https://crates.io/crates/shlex/2.0.1) | 2.0.1 | `MIT OR Apache-2.0` |
| [smallvec](https://crates.io/crates/smallvec/1.16.4) | 1.16.4 | `MIT OR Apache-2.0` |
| [spin](https://crates.io/crates/spin/0.9.9) | 0.9.9 | `MIT` |
| [stable_deref_trait](https://crates.io/crates/stable_deref_trait/1.2.1) | 1.2.1 | `MIT OR Apache-2.0` |
| [syn](https://crates.io/crates/syn/2.0.119) | 2.0.119 | `MIT OR Apache-2.0` |
| [syn](https://crates.io/crates/syn/3.0.9) | 3.0.9 | `MIT OR Apache-2.0` |
| [tempfile](https://crates.io/crates/tempfile/3.27.0) | 3.27.0 | `MIT OR Apache-2.0` |
| [thiserror](https://crates.io/crates/thiserror/2.0.21) | 2.0.21 | `MIT OR Apache-2.0` |
| [thiserror-impl](https://crates.io/crates/thiserror-impl/2.0.21) | 2.0.21 | `MIT OR Apache-2.0` |
| [typenum](https://crates.io/crates/typenum/1.20.1) | 1.20.1 | `MIT OR Apache-2.0` |
| [unicode-ident](https://crates.io/crates/unicode-ident/1.0.27) | 1.0.27 | `(MIT OR Apache-2.0) AND Unicode-3.0` |
| [vcpkg](https://crates.io/crates/vcpkg/0.2.15) | 0.2.15 | `MIT/Apache-2.0` |
| [version_check](https://crates.io/crates/version_check/0.9.5) | 0.9.5 | `MIT/Apache-2.0` |
| [winapi](https://crates.io/crates/winapi/0.3.9) | 0.3.9 | `MIT/Apache-2.0` |
| [winapi-i686-pc-windows-gnu](https://crates.io/crates/winapi-i686-pc-windows-gnu/0.4.0) | 0.4.0 | `MIT/Apache-2.0` |
| [winapi-x86_64-pc-windows-gnu](https://crates.io/crates/winapi-x86_64-pc-windows-gnu/0.4.0) | 0.4.0 | `MIT/Apache-2.0` |
| [windows-link](https://crates.io/crates/windows-link/0.2.1) | 0.2.1 | `MIT OR Apache-2.0` |
| [windows-sys](https://crates.io/crates/windows-sys/0.61.2) | 0.61.2 | `MIT OR Apache-2.0` |
| [zmij](https://crates.io/crates/zmij/1.0.23) | 1.0.23 | `MIT` |
