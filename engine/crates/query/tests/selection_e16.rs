use canonical_query::*;
use canonical_registry::*;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    collections::BTreeSet,
};
thread_local! { static MAX_REQUEST:Cell<usize>=const{Cell::new(0)};  static ACTIVE:Cell<bool>=const{Cell::new(false)}; static CALLS:Cell<usize>=const{Cell::new(0)}; }
struct Count;
fn track(bytes: usize) {
    ACTIVE.with(|a| {
        if a.get() {
            CALLS.with(|c| c.set(c.get() + 1));
            MAX_REQUEST.with(|m| m.set(m.get().max(bytes)));
        }
    });
}
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        track(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        track(n);
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static ALLOC: Count = Count;
#[test]
fn equality_selection_does_not_allocate_a_posting_tree_copy() {
    let d = tempfile::tempdir().unwrap();
    let mut r = Registry::open(d.path()).unwrap();
    r.register_namespace(Namespace {
        id: 1,
        name: "n".into(),
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
        fields: vec![
            Field {
                label: "action".into(),
                kind: FieldType::UInt,
                optional: false,
            },
            Field {
                label: "objective".into(),
                kind: FieldType::UInt,
                optional: false,
            },
        ],
    })
    .unwrap();
    let records = (1..=1000)
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr,
            values: vec![Some(Value::UInt(id % 2)), Some(Value::UInt(id % 4))],
        })
        .collect::<Vec<_>>();
    r.create_batch([1; 16], &records).unwrap();
    let sr2 = SchemaRef { version: 2, ..sr };
    r.register_schema(Schema {
        reference: sr2,
        fields: r.schema(sr).unwrap().fields.clone(),
    })
    .unwrap();
    let second = (1001..=2000)
        .map(|id| NewRecord {
            namespace: 1,
            id,
            schema: sr2,
            values: vec![Some(Value::UInt(id % 2)), Some(Value::UInt(id % 4))],
        })
        .collect::<Vec<_>>();
    r.create_batch([2; 16], &second).unwrap();
    let mut q = QueryEngine::new(&r, r.snapshot()).unwrap();
    let subject = Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    };
    let eq = |schema, position| Predicate::Eq(FieldRef { schema, position }, Value::UInt(0));
    let conjunction = |schema| Predicate::And(vec![eq(schema, 0), eq(schema, 1)]);
    for (predicate, expected) in [
        (
            eq(sr, 0),
            (1..=1000).filter(|id| id % 2 == 0).collect::<Vec<u64>>(),
        ),
        (
            conjunction(sr),
            (1..=1000).filter(|id| id % 4 == 0).collect(),
        ),
        (
            Predicate::Or(vec![conjunction(sr), conjunction(sr2)]),
            (1..=2000).filter(|id| id % 4 == 0).collect(),
        ),
    ] {
        let query = Query {
            namespaces: vec![1],
            schemas: vec![sr, sr2],
            filter: predicate,
            order: Order::default(),
            limit: 1000,
            cursor: None,
        };
        CALLS.with(|c| c.set(0));
        ACTIVE.with(|a| a.set(true));
        let page = q.query_ids(&subject, &query).unwrap();
        ACTIVE.with(|a| a.set(false));
        let calls = CALLS.with(Cell::get);
        eprintln!(
            "selection {:?}: allocations={calls}, rows={}",
            query.filter,
            page.ids.len()
        );
        assert_eq!(
            page.ids.iter().map(|row| row.id).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(page.plan.candidates, page.ids.len());
        assert!(page.ids.windows(2).all(|w| w[0].id < w[1].id));
        assert!(
            calls <= 16,
            "selection allocates {calls} times; a borrowed posting needs only bounded vectors/output"
        );
    }
    for (schemas, namespaces, filter, expected_count) in [
        (vec![sr; 256], vec![1], Predicate::True, 1000),
        (vec![], vec![1; 256], Predicate::True, 1000),
        (
            vec![sr, sr2],
            vec![1],
            Predicate::Or(vec![eq(sr, 0); 255]),
            500,
        ),
    ] {
        let query = Query {
            schemas,
            namespaces,
            filter,
            order: Order::default(),
            limit: 1000,
            cursor: None,
        };
        MAX_REQUEST.with(|m| m.set(0));
        ACTIVE.with(|a| a.set(true));
        let page = q.query_ids(&subject, &query).unwrap();
        ACTIVE.with(|a| a.set(false));
        let largest = MAX_REQUEST.with(Cell::get);
        assert_eq!(page.ids.len(), expected_count);
        let expected = if matches!(query.filter, Predicate::True) {
            (1..=1000).collect::<Vec<u64>>()
        } else {
            (1..=1000).filter(|id| id % 2 == 0).collect()
        };
        assert_eq!(
            page.ids.iter().map(|row| row.id).collect::<Vec<_>>(),
            expected
        );
        let probes = match &query.filter {
            Predicate::True => query.schemas.len() as u64,
            Predicate::Or(children) => children.len() as u64 + query.schemas.len() as u64,
            _ => unreachable!(),
        };
        assert_eq!(page.plan.index_lookups, probes);
        eprintln!(
            "duplicate schemas={} namespaces={} largest_buffer={largest}B rows={}",
            query.schemas.len(),
            query.namespaces.len(),
            page.ids.len()
        );
        assert!(
            largest <= 256_000,
            "duplicate sources requested a {largest} byte buffer for only2000 rows"
        );
    }
}
