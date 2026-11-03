use super::*;
use serde::de::{self, DeserializeSeed, EnumAccess, SeqAccess, VariantAccess, Visitor};
use std::fmt;

/// Schema/namespace metadata frozen independently of storage and registry mutation.
pub struct ProjectionDecoder {
    schemas: BTreeMap<SchemaRef, Schema>,
    namespaces: BTreeSet<u64>,
}
impl ProjectionDecoder {
    pub(crate) fn new(state: &State) -> Self {
        Self {
            schemas: state.schemas.clone(),
            namespaces: state.namespaces.keys().copied().collect(),
        }
    }
    pub fn schema(&self, schema: SchemaRef) -> Result<&Schema> {
        self.schemas.get(&schema).ok_or(Error::UnknownSchema)
    }
    pub fn schemas(&self) -> impl Iterator<Item = &Schema> {
        self.schemas.values()
    }
    /// Return requested fields in request order; omitted fields are still fully validated.
    /// An empty selection validates the complete payload without constructing field values.
    pub fn decode_selected(
        &self,
        schema: SchemaRef,
        payload: &[u8],
        positions: &[usize],
    ) -> Result<Vec<Option<Value>>> {
        let fields = &self.schema(schema)?.fields;
        if positions.len() > MAX_FIELDS {
            return Err(Error::TooLarge);
        }
        let mut seen = [false; MAX_FIELDS];
        for &p in positions {
            if p >= fields.len() {
                return Err(invalid("field position out of bounds"));
            }
            if seen[p] {
                return Err(invalid("duplicate field position"));
            }
            seen[p] = true;
        }
        self.decode_at(schema, payload, positions, 0)
    }
    fn decode_at(
        &self,
        schema: SchemaRef,
        payload: &[u8],
        positions: &[usize],
        depth: usize,
    ) -> Result<Vec<Option<Value>>> {
        if depth > MAX_DEPTH {
            return Err(invalid("value depth exceeded"));
        }
        let schema = self.schema(schema)?;
        canonical_codec::decode_seed(
            payload,
            PayloadSeed {
                decoder: self,
                schema,
                positions,
                depth,
            },
        )
    }
    fn valid_ref(&self, r: Reference) -> bool {
        r.id != 0 && self.namespaces.contains(&r.namespace)
    }
}
pub(super) struct BorrowBytes;
impl<'de> DeserializeSeed<'de> for BorrowBytes {
    type Value = &'de [u8];
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        struct Bytes;
        impl<'de> Visitor<'de> for Bytes {
            type Value = &'de [u8];
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("borrowed bytes")
            }
            fn visit_borrowed_bytes<E: de::Error>(
                self,
                v: &'de [u8],
            ) -> std::result::Result<Self::Value, E> {
                Ok(v)
            }
        }
        d.deserialize_bytes(Bytes)
    }
}
struct BorrowStr;
impl<'de> DeserializeSeed<'de> for BorrowStr {
    type Value = &'de str;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        struct Text;
        impl<'de> Visitor<'de> for Text {
            type Value = &'de str;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("borrowed string")
            }
            fn visit_borrowed_str<E: de::Error>(
                self,
                v: &'de str,
            ) -> std::result::Result<Self::Value, E> {
                Ok(v)
            }
        }
        d.deserialize_str(Text)
    }
}
struct PayloadSeed<'a> {
    decoder: &'a ProjectionDecoder,
    schema: &'a Schema,
    positions: &'a [usize],
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for PayloadSeed<'_> {
    type Value = Vec<Option<Value>>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_struct("Payload", &["bitmap", "values"], self)
    }
}
impl<'de> Visitor<'de> for PayloadSeed<'_> {
    type Value = Vec<Option<Value>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("positional payload")
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let bitmap = seq
            .next_element_seed(BorrowBytes)?
            .ok_or_else(|| de::Error::custom("missing bitmap"))?;
        let count = self.schema.fields.iter().filter(|f| f.optional).count();
        if bitmap.len() != count.div_ceil(8)
            || (!count.is_multiple_of(8) && bitmap.last().is_some_and(|b| *b >> (count % 8) != 0))
        {
            return Err(de::Error::custom("invalid bitmap"));
        }
        seq.next_element_seed(FieldsSeed {
            payload: self,
            bitmap,
        })?
        .ok_or_else(|| de::Error::custom("missing values"))
    }
}
struct FieldsSeed<'a, 'de> {
    payload: PayloadSeed<'a>,
    bitmap: &'de [u8],
}
impl<'de> DeserializeSeed<'de> for FieldsSeed<'_, 'de> {
    type Value = Vec<Option<Value>>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for FieldsSeed<'_, 'de> {
    type Value = Vec<Option<Value>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("positional values")
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let p = self.payload;
        let mut out = vec![None; p.positions.len()];
        let mut optional = 0;
        for (i, field) in p.schema.fields.iter().enumerate() {
            let present = if field.optional {
                let present = self.bitmap[optional / 8] & (1 << (optional % 8)) != 0;
                optional += 1;
                present
            } else {
                true
            };
            if present {
                let slot = p.positions.iter().position(|x| *x == i);
                let value = seq
                    .next_element_seed(ValueSeed {
                        decoder: p.decoder,
                        kind: &field.kind,
                        capture: slot.is_some(),
                        depth: p.depth,
                    })?
                    .ok_or_else(|| de::Error::custom("missing positional value"))?;
                if let Some(slot) = slot {
                    out[slot] = value;
                }
            }
        }
        // Postcard advertises the remaining sequence length. Reject excess without decoding/allocating it.
        if seq.size_hint() != Some(0) {
            return Err(de::Error::custom("excess positional value"));
        }
        Ok(out)
    }
}
struct ValueSeed<'a> {
    decoder: &'a ProjectionDecoder,
    kind: &'a FieldType,
    capture: bool,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for ValueSeed<'_> {
    type Value = Option<Value>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_enum("Value", &[], self)
    }
}
impl<'de> Visitor<'de> for ValueSeed<'_> {
    type Value = Option<Value>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("typed value")
    }
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> std::result::Result<Self::Value, A::Error> {
        let (tag, v) = data.variant::<u32>()?;
        let c = self.capture;
        let value = match (tag, self.kind) {
            (0, FieldType::Bool) => Some(Value::Bool(v.newtype_variant()?)),
            (1, FieldType::UInt) => Some(Value::UInt(v.newtype_variant()?)),
            (2, FieldType::Int) => Some(Value::Int(v.newtype_variant()?)),
            (3, FieldType::Decimal { scale }) => {
                let (m, s): (i64, u8) = v.newtype_variant()?;
                if s != *scale {
                    return Err(de::Error::custom("decimal scale"));
                }
                Some(Value::Decimal {
                    mantissa: m,
                    scale: s,
                })
            }
            (4, FieldType::Float) => {
                let f: f64 = v.newtype_variant()?;
                if !f.is_finite() {
                    return Err(de::Error::custom("nonfinite float"));
                }
                Some(Value::Float(f))
            }
            (5, FieldType::Enum { variants }) => {
                let n: u32 = v.newtype_variant()?;
                if !variants.contains(&n) {
                    return Err(de::Error::custom("enum variant"));
                }
                Some(Value::Enum(n))
            }
            (6, FieldType::Ref) => {
                let r = v.newtype_variant()?;
                if !self.decoder.valid_ref(r) {
                    return Err(de::Error::custom("reference"));
                }
                Some(Value::Ref(r))
            }
            (7, FieldType::Utf8) => {
                let s = v.newtype_variant_seed(BorrowStr)?;
                if s.len() > MAX_TEXT_BYTES {
                    return Err(de::Error::custom("text size"));
                }
                c.then(|| Value::Utf8(s.to_owned()))
            }
            (8, FieldType::Bytes) => {
                let b = v.newtype_variant_seed(BorrowBytes)?;
                if b.len() > MAX_TEXT_BYTES {
                    return Err(de::Error::custom("byte size"));
                }
                c.then(|| Value::Bytes(b.to_vec()))
            }
            (9, FieldType::Array { element, max_items }) => v
                .newtype_variant_seed(ArraySeed {
                    decoder: self.decoder,
                    element,
                    max_items: *max_items,
                    capture: c,
                })?
                .map(Value::Array),
            (10, FieldType::Struct(sr)) => {
                let b = v.newtype_variant_seed(BorrowBytes)?;
                self.decoder
                    .decode_at(*sr, b, &[], self.depth + 1)
                    .map_err(de::Error::custom)?;
                c.then(|| Value::Struct(b.to_vec()))
            }
            (11, FieldType::Relation) => {
                let (source, kind, target): (Reference, u64, Reference) = v.newtype_variant()?;
                if kind == 0 || !self.decoder.valid_ref(source) || !self.decoder.valid_ref(target) {
                    return Err(de::Error::custom("relation"));
                }
                Some(Value::Relation {
                    source,
                    kind,
                    target,
                })
            }
            (12, FieldType::FixedInt) => {
                let b: [u8; 8] = v.newtype_variant()?;
                Some(Value::FixedInt(i64::from_le_bytes(b)))
            }
            _ => return Err(de::Error::custom("field type or value invalid")),
        };
        Ok(if c { value } else { None })
    }
}
struct ArraySeed<'a> {
    decoder: &'a ProjectionDecoder,
    element: &'a ScalarType,
    max_items: usize,
    capture: bool,
}
impl<'de> DeserializeSeed<'de> for ArraySeed<'_> {
    type Value = Option<Vec<ScalarValue>>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for ArraySeed<'_> {
    type Value = Option<Vec<ScalarValue>>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded scalar array")
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        if seq
            .size_hint()
            .is_none_or(|n| n > self.max_items || n > MAX_ARRAY)
        {
            return Err(de::Error::custom("array limit"));
        }
        let mut out = self.capture.then(Vec::new);
        let mut count = 0;
        while let Some(value) = seq.next_element_seed(ScalarSeed {
            decoder: self.decoder,
            kind: self.element,
            capture: self.capture,
        })? {
            count += 1;
            if count > self.max_items {
                return Err(de::Error::custom("array limit"));
            }
            if let (Some(out), Some(value)) = (&mut out, value) {
                out.push(value);
            }
        }
        Ok(out)
    }
}
struct ScalarSeed<'a> {
    decoder: &'a ProjectionDecoder,
    kind: &'a ScalarType,
    capture: bool,
}
impl<'de> DeserializeSeed<'de> for ScalarSeed<'_> {
    type Value = Option<ScalarValue>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_enum("ScalarValue", &[], self)
    }
}
impl<'de> Visitor<'de> for ScalarSeed<'_> {
    type Value = Option<ScalarValue>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("typed scalar")
    }
    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> std::result::Result<Self::Value, A::Error> {
        let (tag, v) = data.variant::<u32>()?;
        let c = self.capture;
        let value = match (tag, self.kind) {
            (0, ScalarType::Bool) => Some(ScalarValue::Bool(v.newtype_variant()?)),
            (1, ScalarType::UInt) => Some(ScalarValue::UInt(v.newtype_variant()?)),
            (2, ScalarType::Int) => Some(ScalarValue::Int(v.newtype_variant()?)),
            (3, ScalarType::Utf8) => {
                let s = v.newtype_variant_seed(BorrowStr)?;
                if s.len() > MAX_TEXT_BYTES {
                    return Err(de::Error::custom("text size"));
                }
                c.then(|| ScalarValue::Utf8(s.to_owned()))
            }
            (4, ScalarType::Bytes) => {
                let b = v.newtype_variant_seed(BorrowBytes)?;
                if b.len() > MAX_TEXT_BYTES {
                    return Err(de::Error::custom("byte size"));
                }
                c.then(|| ScalarValue::Bytes(b.to_vec()))
            }
            (5, ScalarType::Ref) => {
                let r = v.newtype_variant()?;
                if !self.decoder.valid_ref(r) {
                    return Err(de::Error::custom("reference"));
                }
                Some(ScalarValue::Ref(r))
            }
            _ => return Err(de::Error::custom("array scalar type invalid")),
        };
        Ok(if c { value } else { None })
    }
}
