#![cfg(feature = "memory-profile")]
use canonical_memory::{Component, TrackingAllocator};
use canonical_query::*;
use canonical_registry::*;
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator::new(std::alloc::System);
#[test]
fn actual_tracking_allocator_separates_query_rows_scalars_indices_and_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Registry::open(dir.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "query-profile".into(),
        level: Level::Local,
    })
    .unwrap();
    let sr = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    r.register_schema(Schema {
        reference: sr,
        fields: vec![Field {
            label: "text".into(),
            kind: FieldType::Utf8,
            optional: false,
        }],
    })
    .unwrap();
    for id in 1..=2 {
        r.create(1, id, sr, &[Some(Value::Utf8("alpha".into()))])
            .unwrap();
    }
    canonical_memory::clear_census();
    let mut q = QueryEngine::new(&r, r.snapshot()).unwrap();
    let census = canonical_memory::census_snapshot();
    assert_eq!(census["query.rows.count"], 2);
    assert_eq!(census["query.index.ordinal_layout"], 1);
    assert_eq!(census["query.index.legacy_tree_instances"], 0);
    assert_eq!(
        census["query.inline.row_ordinal_size"],
        std::mem::size_of::<usize>() as u64
    );
    assert_eq!(
        census["query.index.ordinal_element_capacity_bytes"],
        6 * std::mem::size_of::<usize>() as u64
    );
    assert_eq!(census["query.secondary.key_heap_capacity_bytes"], 0);
    assert_eq!(census["query.rows.scalar_heap_len_bytes"], 10);
    assert_eq!(
        census["query.secondary.key_heap_len_bytes"], 0,
        "ordinal index must borrow canonical Scalars rather than own a cloned heap key"
    );
    let counts = canonical_memory::snapshot();
    for c in [
        Component::QueryRows,
        Component::QueryScalars,
        Component::QueryIndexes,
    ] {
        assert!(
            counts.components[c as usize].current_requested > 0,
            "{}",
            c.name()
        );
    }
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
    assert_eq!(
        canonical_memory::census_snapshot()["query.cursors.count"],
        1
    );
    assert!(
        canonical_memory::snapshot().components[Component::QueryCursors as usize].current_requested
            > 0
    );
}
