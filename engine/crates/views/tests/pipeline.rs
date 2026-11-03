use canonical_core::{Error, Result};
use canonical_query::*;
use canonical_registry::*;
use canonical_views::*;
use std::{
    cell::Cell,
    collections::BTreeSet,
    io::{self, Write},
    rc::Rc,
};

struct Pages<'a> {
    engine: &'a mut QueryEngine,
    subject: &'a Subject,
    request: Query,
    buffer: std::vec::IntoIter<Record>,
    done: bool,
    calls: Rc<Cell<usize>>,
}
impl Iterator for Pages<'_> {
    type Item = Result<Record>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(record) = self.buffer.next() {
                return Some(Ok(record));
            }
            if self.done {
                return None;
            }
            self.calls.set(self.calls.get() + 1);
            match self.engine.query(self.subject, &self.request) {
                Ok(page) => {
                    self.request.cursor = page.next;
                    self.done = self.request.cursor.is_none();
                    self.buffer = page.records.into_iter();
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}
fn pages<'a>(
    engine: &'a mut QueryEngine,
    subject: &'a Subject,
    request: Query,
    calls: Rc<Cell<usize>>,
) -> Pages<'a> {
    Pages {
        engine,
        subject,
        request,
        buffer: Vec::new().into_iter(),
        done: false,
        calls,
    }
}
fn sr(id: u64) -> SchemaRef {
    SchemaRef {
        namespace: 1,
        id,
        version: 1,
    }
}
fn subject() -> Subject {
    Subject {
        id: "reader".into(),
        readable_namespaces: BTreeSet::from([1]),
    }
}
fn request(schema: SchemaRef) -> Query {
    Query {
        namespaces: vec![1],
        schemas: vec![schema],
        filter: Predicate::True,
        order: Order::default(),
        limit: 1,
        cursor: None,
    }
}
fn setup(path: &std::path::Path) -> Registry {
    let mut r = Registry::open(path).unwrap();
    for id in [1, 2] {
        r.register_namespace(Namespace {
            id,
            name: format!("ns{id}"),
            level: Level::Local,
        })
        .unwrap();
    }
    r.register_schema(Schema {
        reference: sr(1),
        fields: vec![Field {
            label: "value".into(),
            kind: FieldType::UInt,
            optional: false,
        }],
    })
    .unwrap();
    r.create_batch(
        [1; 16],
        &(1..=3)
            .map(|id| NewRecord {
                namespace: 1,
                id,
                schema: sr(1),
                values: vec![Some(Value::UInt(id))],
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    r
}

#[test]
fn paginated_authorized_query_streams_pages_without_preparing_them() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let who = subject();
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let calls = Rc::new(Cell::new(0));
    let source = pages(&mut engine, &who, request(sr(1)), Rc::clone(&calls));
    assert_eq!(calls.get(), 0);
    let mut output = Vec::new();
    let stats = reader
        .write_many(Format::Json, source, &mut output)
        .unwrap();
    let array: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(array.as_array().unwrap().len(), 3);
    assert_eq!(stats.records, 3);
    assert_eq!(calls.get(), 3);
}

struct Broken;
impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "stop before query",
        ))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn sink_failure_before_json_prefix_does_not_execute_a_query() {
    let dir = tempfile::tempdir().unwrap();
    let r = setup(dir.path());
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let who = subject();
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let calls = Rc::new(Cell::new(0));
    assert!(matches!(
        reader.write_many(
            Format::Json,
            pages(&mut engine, &who, request(sr(1)), Rc::clone(&calls)),
            &mut Broken
        ),
        Err(Error::Io(_))
    ));
    assert_eq!(calls.get(), 0);
}

#[test]
fn all_formats_preserve_the_authorized_query_boundary_for_native_edges() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = setup(dir.path());
    r.create(2, 1, sr(1), &[Some(Value::UInt(99))]).unwrap();
    r.register_schema(Schema {
        reference: sr(2),
        fields: vec![Field {
            label: "edge".into(),
            kind: FieldType::Relation,
            optional: false,
        }],
    })
    .unwrap();
    r.register_relation_rule(RelationRule {
        schema: sr(2),
        kind: 10,
        source_schema: sr(1),
        target_schema: sr(1),
    })
    .unwrap();
    r.create(
        1,
        100,
        sr(2),
        &[Some(Value::Relation {
            source: Reference {
                namespace: 1,
                id: 1,
            },
            kind: 10,
            target: Reference {
                namespace: 2,
                id: 1,
            },
        })],
    )
    .unwrap();
    let mut engine = QueryEngine::new(&r, r.snapshot()).unwrap();
    let reader = Reader::new(&r, Limits::default()).unwrap();
    let who = subject();
    for format in [
        Format::Json,
        Format::Csv {
            schema: sr(2),
            nested: CsvNestedPolicy::JsonCell,
        },
        Format::Cbr,
    ] {
        let calls = Rc::new(Cell::new(0));
        let mut output = Vec::new();
        let stats = reader
            .write_many(
                format,
                pages(&mut engine, &who, request(sr(2)), calls),
                &mut output,
            )
            .unwrap();
        assert_eq!(stats.records, 0);
        match format {
            Format::Json => assert_eq!(output, b"[]"),
            Format::Cbr => assert!(output.is_empty()),
            Format::Csv { .. } => assert_eq!(
                String::from_utf8(output).unwrap().matches("\r\n").count(),
                1
            ),
        }
    }
}
