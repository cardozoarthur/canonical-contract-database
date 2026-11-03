#![cfg(feature = "memory-profile")]
use canonical_memory::{Component, TrackingAllocator, census_snapshot, clear_census, snapshot};
use canonical_registry::*;
use canonical_relations::{GraphLayout, GraphView};
#[global_allocator]
static ALLOC: TrackingAllocator = TrackingAllocator::new(std::alloc::System);
#[test]
fn graph_census_counts_all_snapshot_identities_and_both_adjacencies() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "graph-memory".into(),
        level: Level::Local,
    })
    .unwrap();
    let node = SchemaRef {
        namespace: 1,
        id: 1,
        version: 1,
    };
    let edge = SchemaRef {
        namespace: 1,
        id: 2,
        version: 1,
    };
    r.register_schema(Schema {
        reference: node,
        fields: vec![],
    })
    .unwrap();
    r.register_schema(Schema {
        reference: edge,
        fields: vec![Field {
            label: "edge".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.register_relation_rule(RelationRule {
        schema: edge,
        kind: 1,
        source_schema: node,
        target_schema: node,
    })
    .unwrap();
    r.create(1, 1, node, &[]).unwrap();
    r.create(1, 2, node, &[]).unwrap();
    r.create(
        1,
        3,
        edge,
        &[Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 1,
            target: Reference {
                namespace: 1,
                id: 2,
            },
        })],
    )
    .unwrap();
    for layout in [GraphLayout::Legacy, GraphLayout::Compact] {
        let snap = r.snapshot();
        clear_census();
        let g = GraphView::new_with_layout(&r, snap, layout).unwrap();
        let c = census_snapshot();
        assert_eq!(c["graph.nodes_count"], 3);
        assert_eq!(c["graph.edges_count"], 1);
        assert_eq!(c["graph.adjacency_memberships"], 2);
        assert!(c["graph.adjacency_capacity_bytes"] >= 2 * std::mem::size_of::<Reference>() as u64);
        let s = snapshot();
        for component in [
            Component::GraphNodes,
            Component::GraphEdges,
            Component::GraphAdjacency,
        ] {
            assert!(s.components[component as usize].current_requested > 0);
        }
        if layout == GraphLayout::Compact {
            assert_eq!(c["graph.layout_compact"], 1);
            assert_eq!(
                c["graph.compact.offsets.len_bytes"],
                8 * std::mem::size_of::<usize>() as u64
            );
            assert_eq!(
                c["graph.compact.postings.len_bytes"],
                2 * std::mem::size_of::<usize>() as u64
            );
            assert_eq!(c["graph.compact.cursor_scratch_retained_bytes"], 0);
            assert!(c["graph.compact.edges.capacity_bytes"] > 0);
        }
        drop(g);
        let freed = snapshot();
        for component in [
            Component::GraphNodes,
            Component::GraphEdges,
            Component::GraphAdjacency,
        ] {
            assert_eq!(freed.components[component as usize].current_requested, 0);
        }
    }
}
