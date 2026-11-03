//! A finite, destructive-maintenance demonstration using a NEW directory only.
use canonical_registry::*;
use std::{error::Error, fs, path::Path, time::Instant};
fn bytes(path: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let t = entry.file_type()?;
        if t.is_dir() {
            total += bytes(&entry.path())?;
        } else if t.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}
fn input(sr: SchemaRef, id: u64, revision: u64) -> NewRecord {
    NewRecord {
        namespace: 1,
        id,
        schema: sr,
        values: vec![Some(Value::Utf8(format!(
            "fact {id} revision {revision}: {}",
            "typed evidence; ".repeat(128)
        )))],
    }
}
fn tx(revision: u64) -> [u8; 16] {
    let mut id = [0; 16];
    id[..8].copy_from_slice(&revision.to_le_bytes());
    id
}
fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: compact NEW_DIRECTORY")?;
    let path = Path::new(&path);
    fs::create_dir(path)?; // Refuse an existing directory; never reuse arbitrary databases.
    let mut r = Registry::open(path)?;
    r.register_namespace(Namespace {
        id: 1,
        name: "experiment".into(),
        level: Level::Local,
    })?;
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: vec![Field {
            label: "evidence".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })?;
    let rows = 256;
    let versions = 3;
    r.create_batch(
        tx(1),
        &(1..=rows).map(|id| input(sr, id, 1)).collect::<Vec<_>>(),
    )?;
    for revision in 2..=versions {
        r.commit(
            tx(revision),
            &(1..=rows)
                .map(|id| Mutation::Update {
                    record: input(sr, id, revision),
                    expected_revision: revision - 1,
                })
                .collect::<Vec<_>>(),
        )?;
    }
    r.flush_index()?;
    let before = bytes(path)?;
    let before_seq = r.snapshot().commit_sequence;
    let started = Instant::now();
    let all = r.compact(Retention::KeepAll)?;
    let all_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(all.pruned_revisions, 0);
    assert_eq!(all.retained_revisions, 768);
    r.flush_index()?;
    let keep_all_total = bytes(path)?;
    let started = Instant::now();
    let retained = r.compact(Retention::KeepLast(1))?;
    let prune_ms = started.elapsed().as_secs_f64() * 1000.0;
    r.flush_index()?;
    assert_eq!(retained.pruned_revisions, 512);
    assert_eq!(r.snapshot().commit_sequence, before_seq);
    let with_backups = bytes(path)?;
    let cleanup = r.cleanup_obsolete_generations()?;
    let after_cleanup = bytes(path)?;
    drop(r);
    let mut r = Registry::open(path)?;
    for id in 1..=rows {
        let v = r
            .lookup(1, id, None)?
            .revision
            .ok_or("missing retained head")?;
        assert_eq!(v.record.revision, 3);
        assert_eq!(
            r.decode_values(v.record.schema, &v.record.payload)?,
            input(sr, id, 3).values
        );
        assert_eq!(r.history(1, id).len(), 1);
    }
    println!("# E09: experimento local de compactação\n");
    println!(
        "256 identidades × 3 revisões; strings sintéticas de aproximadamente 2 KiB. Reopen e 256 valores finais verificados. Sequência mantida: {before_seq}. Sem comparação de recursos com outros bancos nesta poda explícita.\n"
    );
    println!(
        "| Medida | Valor |\n|---|---:|\n| Diretório antes (bytes) | {before} |\n| KeepAll ativo (bytes) | {} |\n| KeepAll diretório com backup (bytes) | {keep_all_total} |\n| KeepAll ms | {all_ms:.3} |\n| KeepLast ativo (bytes) | {} |\n| KeepLast diretório com backups (bytes) | {with_backups} |\n| KeepLast ms | {prune_ms:.3} |\n| Revisões podadas | {} |\n| Bytes de backups removidos explicitamente | {} |\n| Diretório após cleanup (bytes) | {after_cleanup} |",
        all.generation.active_bytes,
        retained.generation.active_bytes,
        retained.pruned_revisions,
        cleanup.removed_bytes
    );
    println!(
        "\nKeepAll preserva o histórico e cria backup, podendo aumentar uso total. Poda reduz apenas payloads elegíveis: metadados/recibos/commits permanecem. Os totais incluem registry, índice e gerações. Valores de tempo incluem revalidação e sync em cache local; não são medição física de energia ou garantia de produção."
    );
    Ok(())
}
