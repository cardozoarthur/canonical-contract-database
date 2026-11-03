use canonical_core::{Error, Result};
use canonical_registry::{
    FieldType, MAX_DEPTH, Record, Reference, Registry, ScalarValue, SchemaRef, Value,
};
use serde::{
    Serialize, Serializer,
    ser::{Error as _, SerializeMap, SerializeSeq},
};
use std::io::{self, Write};

pub(super) fn write<W: Write, T: Serialize + ?Sized>(out: &mut W, value: &T) -> Result<()> {
    serde_json::to_writer(out, value).map_err(|e| match e.io_error_kind() {
        Some(kind) => Error::Io(io::Error::new(kind, e)),
        None => Error::InvalidInput("JSON view serialization failed"),
    })
}
pub(super) struct JsonRecord<'a> {
    pub registry: &'a Registry,
    pub record: &'a Record,
    pub values: &'a [Option<Value>],
}
impl Serialize for JsonRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("namespace", &self.record.namespace)?;
        map.serialize_entry("id", &self.record.id)?;
        map.serialize_entry("schema", &Schema(self.record.schema))?;
        map.serialize_entry("revision", &self.record.revision)?;
        map.serialize_entry(
            "fields",
            &Fields {
                registry: self.registry,
                schema: self.record.schema,
                values: self.values,
                depth: 0,
            },
        )?;
        map.end()
    }
}
struct Schema(SchemaRef);
impl Serialize for Schema {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(3))?;
        map.serialize_entry("namespace", &self.0.namespace)?;
        map.serialize_entry("id", &self.0.id)?;
        map.serialize_entry("version", &self.0.version)?;
        map.end()
    }
}
struct Ref(Reference);
impl Serialize for Ref {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("namespace", &self.0.namespace)?;
        map.serialize_entry("id", &self.0.id)?;
        map.end()
    }
}
struct Fields<'a> {
    registry: &'a Registry,
    schema: SchemaRef,
    values: &'a [Option<Value>],
    depth: usize,
}
impl Serialize for Fields<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        if self.depth > MAX_DEPTH {
            return Err(S::Error::custom("view nesting depth exceeded"));
        }
        let schema = self
            .registry
            .schema(self.schema)
            .map_err(S::Error::custom)?;
        if schema.fields.len() != self.values.len() {
            return Err(S::Error::custom("view field cardinality mismatch"));
        }
        let mut map = s.serialize_map(Some(schema.fields.len()))?;
        for (field, value) in schema.fields.iter().zip(self.values) {
            match value {
                None => map.serialize_entry(&field.label, &Option::<u8>::None)?,
                Some(value) => map.serialize_entry(
                    &field.label,
                    &Typed {
                        registry: self.registry,
                        kind: &field.kind,
                        value,
                        depth: self.depth,
                    },
                )?,
            };
        }
        map.end()
    }
}
pub(super) struct Typed<'a> {
    pub registry: &'a Registry,
    pub kind: &'a FieldType,
    pub value: &'a Value,
    pub depth: usize,
}
impl Serialize for Typed<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.value {
            Value::Bool(v) => s.serialize_bool(*v),
            Value::UInt(v) => s.serialize_u64(*v),
            Value::Int(v) | Value::FixedInt(v) => s.serialize_i64(*v),
            Value::Float(v) => s.serialize_f64(*v),
            Value::Enum(v) => s.serialize_u32(*v),
            Value::Utf8(v) => s.serialize_str(v),
            Value::Bytes(v) => v.serialize(s),
            Value::Ref(v) => Ref(*v).serialize(s),
            Value::Decimal { mantissa, scale } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("mantissa", mantissa)?;
                map.serialize_entry("scale", scale)?;
                map.end()
            }
            Value::Relation {
                source,
                kind,
                target,
            } => {
                let mut map = s.serialize_map(Some(3))?;
                map.serialize_entry("source", &Ref(*source))?;
                map.serialize_entry("kind", kind)?;
                map.serialize_entry("target", &Ref(*target))?;
                map.end()
            }
            Value::Array(values) => {
                let mut seq = s.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&Scalar(value))?;
                }
                seq.end()
            }
            Value::Struct(bytes) => {
                let FieldType::Struct(child) = self.kind else {
                    return Err(S::Error::custom("struct schema missing"));
                };
                if self.depth >= MAX_DEPTH {
                    return Err(S::Error::custom("view nesting depth exceeded"));
                }
                let values = self
                    .registry
                    .decode_values(*child, bytes)
                    .map_err(S::Error::custom)?;
                Fields {
                    registry: self.registry,
                    schema: *child,
                    values: &values,
                    depth: self.depth + 1,
                }
                .serialize(s)
            }
        }
    }
}
struct Scalar<'a>(&'a ScalarValue);
impl Serialize for Scalar<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            ScalarValue::Bool(v) => s.serialize_bool(*v),
            ScalarValue::UInt(v) => s.serialize_u64(*v),
            ScalarValue::Int(v) => s.serialize_i64(*v),
            ScalarValue::Utf8(v) => s.serialize_str(v),
            ScalarValue::Bytes(v) => v.serialize(s),
            ScalarValue::Ref(v) => Ref(*v).serialize(s),
        }
    }
}
