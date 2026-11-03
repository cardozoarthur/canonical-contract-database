//! Private opt-in ownership census; no invented B-tree node-size estimates.
use super::*;
use canonical_memory::{Component, census_set, enabled, scope};
fn scalar_heap(value: &Scalar) -> (u64, u64) {
    match value {
        Scalar::Utf8(v) => (v.len() as u64, v.capacity() as u64),
        Scalar::Bytes(v) => (v.len() as u64, v.capacity() as u64),
        _ => (0, 0),
    }
}
pub(super) fn inventory_query(engine: &QueryEngine) -> BTreeMap<&'static str, u64> {
    let _collector = scope(Component::Harness);
    let payload_len = 0;
    let payload_cap = 0;
    let mut shared_len = 0u64;
    let mut shared_cap = 0u64;
    for record in engine.snapshot.iter() {
        shared_len += record.payload.len() as u64;
        shared_cap += record.payload.capacity() as u64;
    }
    let mut scalar_len = 0;
    let mut scalar_cap = 0;
    let mut scalar_elements_cap = 0;
    for row in engine.rows.iter().map(|entry| &entry.row) {
        scalar_elements_cap +=
            (row.values.capacity() * std::mem::size_of::<Option<Scalar>>()) as u64;
        for value in row.values.iter().flatten() {
            let (len, cap) = scalar_heap(value);
            scalar_len += len;
            scalar_cap += cap;
        }
    }
    let key_len = 0;
    let key_cap = 0;
    let mut distinct_keys = 0;
    let mut memberships = 0;
    for (field, ordinals) in &engine.secondary {
        memberships += ordinals.len() as u64;
        if !ordinals.is_empty() {
            distinct_keys += 1;
        }
        distinct_keys += ordinals
            .windows(2)
            .filter(|pair| {
                engine.posting_value(pair[0], *field) != engine.posting_value(pair[1], *field)
            })
            .count() as u64;
    }
    let ordinal_bytes = engine.secondary.values().map(|v| v.len()).sum::<usize>()
        + engine.by_namespace.values().map(|v| v.len()).sum::<usize>()
        + engine.by_schema.values().map(|v| v.len()).sum::<usize>();
    let mut field_cap = 0;
    let mut label_len = 0;
    let mut label_cap = 0;
    let mut enum_cap = 0;
    for schema in engine.decoder.schemas() {
        field_cap +=
            (schema.fields.capacity() * std::mem::size_of::<canonical_registry::Field>()) as u64;
        for f in &schema.fields {
            label_len += f.label.len() as u64;
            label_cap += f.label.capacity() as u64;
            if let FieldType::Enum { variants } = &f.kind {
                enum_cap += (variants.capacity() * std::mem::size_of::<u32>()) as u64;
            }
        }
    }
    BTreeMap::from([
        ("query.census.snapshot_sequence", engine.sequence),
        ("query.index.ordinal_layout", 1),
        ("query.index.legacy_tree_instances", u64::from(cfg!(test))),
        (
            "query.index.ordinal_element_capacity_bytes",
            (ordinal_bytes * std::mem::size_of::<usize>()) as u64,
        ),
        (
            "query.inline.row_ordinal_size",
            std::mem::size_of::<usize>() as u64,
        ),
        (
            "query.rows.element_capacity_bytes",
            (engine.rows.len() * std::mem::size_of::<DenseRow>()) as u64,
        ),
        ("query.rows.count", engine.rows.len() as u64),
        (
            "query.snapshot.shared_records",
            engine.snapshot.len() as u64,
        ),
        ("query.snapshot.shared_payload_len_bytes", shared_len),
        ("query.snapshot.shared_payload_capacity_bytes", shared_cap),
        ("query.rows.payload_len_bytes", payload_len),
        ("query.rows.payload_capacity_bytes", payload_cap),
        (
            "query.rows.scalar_element_capacity_bytes",
            scalar_elements_cap,
        ),
        ("query.rows.scalar_heap_len_bytes", scalar_len),
        ("query.rows.scalar_heap_capacity_bytes", scalar_cap),
        ("query.secondary.fields", engine.secondary.len() as u64),
        ("query.secondary.distinct_keys", distinct_keys),
        ("query.secondary.memberships", memberships),
        ("query.secondary.key_heap_len_bytes", key_len),
        ("query.secondary.key_heap_capacity_bytes", key_cap),
        (
            "query.by_namespace.buckets",
            engine.by_namespace.len() as u64,
        ),
        (
            "query.by_namespace.memberships",
            engine.by_namespace.values().map(|v| v.len() as u64).sum(),
        ),
        ("query.by_schema.buckets", engine.by_schema.len() as u64),
        (
            "query.by_schema.memberships",
            engine.by_schema.values().map(|v| v.len() as u64).sum(),
        ),
        (
            "query.schemas.count",
            engine.decoder.schemas().count() as u64,
        ),
        ("query.schemas.field_element_capacity_bytes", field_cap),
        ("query.schemas.label_len_bytes", label_len),
        ("query.schemas.label_capacity_bytes", label_cap),
        ("query.schemas.enum_element_capacity_bytes", enum_cap),
        ("query.cursors.count", engine.cursors.len() as u64),
        ("query.inline.row_size", std::mem::size_of::<Row>() as u64),
        (
            "query.inline.row_key_size",
            std::mem::size_of::<RowKey>() as u64,
        ),
        (
            "query.inline.scalar_size",
            std::mem::size_of::<Scalar>() as u64,
        ),
        (
            "query.inline.optional_scalar_size",
            std::mem::size_of::<Option<Scalar>>() as u64,
        ),
    ])
}
pub(super) fn publish_query(engine: &QueryEngine) {
    if enabled() {
        for (key, value) in inventory_query(engine) {
            census_set(key, value);
        }
    }
}
/// Hot-path metadata is O(1); full rows/index census is constructor-only.
pub(super) fn publish_cursors(engine: &QueryEngine) {
    if enabled() {
        census_set("query.cursors.count", engine.cursors.len() as u64);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn census_distinguishes_shared_snapshot_from_row_scalar_and_secondary_copies() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Registry::open(dir.path()).unwrap();
        r.register_namespace(canonical_registry::Namespace {
            id: 1,
            name: "query-memory".into(),
            level: canonical_registry::Level::Local,
        })
        .unwrap();
        let sr = SchemaRef {
            namespace: 1,
            id: 1,
            version: 1,
        };
        r.register_schema(Schema {
            reference: sr,
            fields: vec![
                canonical_registry::Field {
                    label: "text".into(),
                    kind: FieldType::Utf8,
                    optional: false,
                },
                canonical_registry::Field {
                    label: "bytes".into(),
                    kind: FieldType::Bytes,
                    optional: false,
                },
            ],
        })
        .unwrap();
        for id in 1..=2 {
            r.create(
                1,
                id,
                sr,
                &[
                    Some(Value::Utf8("alpha".into())),
                    Some(Value::Bytes(vec![1, 2, 3])),
                ],
            )
            .unwrap();
        }
        let mut q = QueryEngine::new(&r, r.snapshot()).unwrap();
        let c = inventory_query(&q);
        assert_eq!(c["query.rows.count"], 2);
        assert_eq!(c["query.rows.scalar_heap_len_bytes"], 16);
        assert_eq!(c["query.secondary.key_heap_len_bytes"], 0);
        assert_eq!(c["query.secondary.memberships"], 4);
        assert_eq!(c["query.rows.payload_capacity_bytes"], 0);
        assert!(c["query.snapshot.shared_payload_capacity_bytes"] > 0);
        assert_eq!(c["query.snapshot.shared_records"], 2);
        let subject = Subject {
            id: "reader".into(),
            readable_namespaces: [1].into_iter().collect(),
        };
        let page = q
            .query(
                &subject,
                &Query {
                    namespaces: vec![1],
                    schemas: vec![],
                    filter: Predicate::True,
                    order: Order::default(),
                    limit: 1,
                    cursor: None,
                },
            )
            .unwrap();
        assert!(page.next.is_some());
        assert_eq!(inventory_query(&q)["query.cursors.count"], 1);
    }
}
