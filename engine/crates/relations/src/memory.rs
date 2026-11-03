//! Last-construction ownership counts. Tree node heap is observed by the allocator.
use super::*;
use canonical_memory::{Component, census_set, enabled, scope};
pub(super) fn publish_graph(graph: &GraphView) {
    if !enabled() {
        return;
    }
    let _collector = scope(Component::Harness);
    let compact = graph.compact.as_ref();
    census_set("graph.layout_compact", u64::from(compact.is_some()));
    census_set(
        "graph.nodes_count",
        compact.map_or(graph.nodes.len(), |c| c.nodes.len()) as u64,
    );
    census_set(
        "graph.edges_count",
        compact.map_or(graph.edges.len(), |c| c.edges.len()) as u64,
    );
    census_set("graph.rules_count", graph.rules.len() as u64);
    census_set(
        "graph.source_buckets",
        compact.map_or(graph.sources.len(), |c| {
            c.sources
                .offsets
                .windows(2)
                .filter(|w| w[0] != w[1])
                .count()
        }) as u64,
    );
    census_set(
        "graph.target_buckets",
        compact.map_or(graph.targets.len(), |c| {
            c.targets
                .offsets
                .windows(2)
                .filter(|w| w[0] != w[1])
                .count()
        }) as u64,
    );
    if let Some(c) = compact {
        census_set(
            "graph.adjacency_memberships",
            (c.sources.postings.len() + c.targets.postings.len()) as u64,
        );
        census_set(
            "graph.adjacency_capacity_bytes",
            ((c.sources.offsets.capacity()
                + c.targets.offsets.capacity()
                + c.sources.postings.capacity()
                + c.targets.postings.capacity())
                * std::mem::size_of::<usize>()) as u64,
        );
        for (len_label, cap_label, len, cap, size) in [
            (
                "graph.compact.nodes.len_bytes",
                "graph.compact.nodes.capacity_bytes",
                c.nodes.len(),
                c.nodes.capacity(),
                std::mem::size_of::<(Reference, SchemaRef)>(),
            ),
            (
                "graph.compact.edges.len_bytes",
                "graph.compact.edges.capacity_bytes",
                c.edges.len(),
                c.edges.capacity(),
                std::mem::size_of::<NativeEdge>(),
            ),
            (
                "graph.compact.offsets.len_bytes",
                "graph.compact.offsets.capacity_bytes",
                c.sources.offsets.len() + c.targets.offsets.len(),
                c.sources.offsets.capacity() + c.targets.offsets.capacity(),
                std::mem::size_of::<usize>(),
            ),
            (
                "graph.compact.postings.len_bytes",
                "graph.compact.postings.capacity_bytes",
                c.sources.postings.len() + c.targets.postings.len(),
                c.sources.postings.capacity() + c.targets.postings.capacity(),
                std::mem::size_of::<usize>(),
            ),
        ] {
            census_set(len_label, (len * size) as u64);
            census_set(cap_label, (cap * size) as u64);
        }
        census_set(
            "graph.compact.cursor_scratch_peak_bytes",
            c.sources
                .scratch_peak_bytes
                .max(c.targets.scratch_peak_bytes) as u64,
        );
        census_set("graph.compact.cursor_scratch_retained_bytes", 0);
    } else {
        // No compact buffers are owned by Legacy; clear prior-construction census values.
        for key in [
            "graph.compact.nodes.len_bytes",
            "graph.compact.nodes.capacity_bytes",
            "graph.compact.edges.len_bytes",
            "graph.compact.edges.capacity_bytes",
            "graph.compact.offsets.len_bytes",
            "graph.compact.offsets.capacity_bytes",
            "graph.compact.postings.len_bytes",
            "graph.compact.postings.capacity_bytes",
            "graph.compact.cursor_scratch_peak_bytes",
            "graph.compact.cursor_scratch_retained_bytes",
        ] {
            census_set(key, 0);
        }
        census_set(
            "graph.adjacency_memberships",
            graph
                .sources
                .values()
                .chain(graph.targets.values())
                .map(|v| v.len() as u64)
                .sum(),
        );
        census_set(
            "graph.adjacency_capacity_bytes",
            graph
                .sources
                .values()
                .chain(graph.targets.values())
                .map(|v| (v.capacity() * std::mem::size_of::<Reference>()) as u64)
                .sum(),
        );
    }
    census_set(
        "graph.native_edge_inline_size_bytes",
        std::mem::size_of::<NativeEdge>() as u64,
    );
    census_set(
        "graph.reference_inline_size_bytes",
        std::mem::size_of::<Reference>() as u64,
    );
    census_set(
        "graph.ordinal_inline_size_bytes",
        std::mem::size_of::<usize>() as u64,
    );
    census_set("graph.fact_payload_copies", 0);
}
