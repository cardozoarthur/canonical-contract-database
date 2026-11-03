// Adapter integration tests; only included where all backend modules exist.
use crate::common::*;
fn exercise(factory: OpenBackend, label: &str, rows: u64) {
    let cfg = Config {
        rows,
        revisions: 11,
        samples: 3,
    };
    let path = std::env::temp_dir().join(format!(
        "e13-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    let mut b = factory(&path, true, cfg).unwrap();
    b.register_schema(1).unwrap();
    for rev in 1..=cfg.revisions {
        b.write_facts(
            &(1..=cfg.rows)
                .map(|id| fact(cfg, id, rev))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    b.register_schema(2).unwrap();
    let null_id = cfg.graph_nodes() + 1;
    let mut migrated = (null_id..=cfg.rows)
        .map(|id| fact(cfg, id, 12))
        .collect::<Vec<_>>();
    migrated[0].extra = None;
    b.write_facts(&migrated).unwrap();
    b.flush().unwrap();
    drop(b);
    let mut b = factory(&path, false, cfg).unwrap();
    b.project().unwrap();
    for (a, o) in [(2, 2), (1 + null_id % 7, 1 + null_id % 17), (999, 999)] {
        let expected = matches(cfg, a, o)
            .into_iter()
            .map(|id| {
                let mut f = fact(cfg, id, cfg.latest(id));
                if id == null_id {
                    f.extra = None;
                }
                f
            })
            .collect::<Vec<_>>();
        assert_eq!(b.filter_facts(a, o).unwrap(), expected);
        for shape in [
            ProjectionShape::Ids,
            ProjectionShape::Fields,
            ProjectionShape::Full,
        ] {
            assert_eq!(
                b.select(a, o, shape).unwrap(),
                project_facts(expected.clone(), shape)
            );
        }
    }
    // v2 optional null and v1 absent share Option::None, but retain different schema identities.
    assert!(
        matches!(b.select(1+null_id%7,1+null_id%17,ProjectionShape::Fields).unwrap(),ProjectionResult::Fields(v) if v.iter().any(|r|r.identity.id==null_id && r.identity.schema_version==2 && r.extra.is_none()))
    );
}
#[test]
fn sqlite_projection_outputs() {
    exercise(crate::sqlite::open, "sqlite", 40);
    exercise(crate::sqlite::open, "sqlite-many", 240);
}
#[test]
fn messagepack_projection_outputs() {
    exercise(crate::codec_log::open_messagepack, "mp", 40);
    exercise(crate::codec_log::open_messagepack, "mp-many", 240);
}
#[test]
fn protobuf_projection_outputs() {
    exercise(crate::codec_log::open_protobuf, "proto", 40);
    exercise(crate::codec_log::open_protobuf, "proto-many", 240);
}
