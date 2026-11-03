use crate::{CsvNestedPolicy, json};
use canonical_core::{Error, Result};
use canonical_registry::{Record, Registry, Schema, Value};
use std::io::{self, Write};

pub(super) fn header<W: Write>(schema: &Schema, out: &mut W) -> Result<()> {
    let metadata = [
        "namespace",
        "id",
        "schema_namespace",
        "schema_id",
        "schema_version",
        "revision",
    ];
    for (i, label) in metadata.into_iter().enumerate() {
        if i > 0 {
            out.write_all(b",")?;
        }
        cell(out, |w| {
            w.write_all(label.as_bytes())?;
            Ok(())
        })?;
    }
    for field in &schema.fields {
        out.write_all(b",")?;
        cell(out, |w| {
            w.write_all(b"field.")?;
            w.write_all(field.label.as_bytes())?;
            Ok(())
        })?;
    }
    out.write_all(b"\r\n")?;
    Ok(())
}
pub(super) fn record<W: Write>(
    registry: &Registry,
    record: &Record,
    values: &[Option<Value>],
    policy: CsvNestedPolicy,
    out: &mut W,
) -> Result<()> {
    let schema = registry.schema(record.schema)?;
    if policy == CsvNestedPolicy::Reject && values.iter().flatten().any(compound) {
        return Err(Error::InvalidInput(
            "CSV nested values require JsonCell policy",
        ));
    }
    for (i, value) in [
        record.namespace,
        record.id,
        record.schema.namespace,
        record.schema.id,
        record.schema.version as u64,
        record.revision,
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            out.write_all(b",")?;
        }
        cell(out, |w| {
            write!(w, "{value}")?;
            Ok(())
        })?;
    }
    for (field, value) in schema.fields.iter().zip(values) {
        out.write_all(b",")?;
        cell(out, |w| {
            let Some(value) = value else {
                return Ok(());
            };
            match value {
                Value::Utf8(v) => w.write_all(v.as_bytes())?,
                Value::Bool(v) => write!(w, "{v}")?,
                Value::UInt(v) => write!(w, "{v}")?,
                Value::Int(v) | Value::FixedInt(v) => write!(w, "{v}")?,
                Value::Enum(v) => write!(w, "{v}")?,
                Value::Decimal { mantissa, scale } => decimal(w, *mantissa, *scale)?,
                Value::Float(v) => json::write(w, v)?,
                _ => json::write(
                    w,
                    &json::Typed {
                        registry,
                        kind: &field.kind,
                        value,
                        depth: 0,
                    },
                )?,
            }
            Ok(())
        })?;
    }
    out.write_all(b"\r\n")?;
    Ok(())
}
fn compound(value: &Value) -> bool {
    matches!(
        value,
        Value::Bytes(_)
            | Value::Ref(_)
            | Value::Array(_)
            | Value::Struct(_)
            | Value::Relation { .. }
    )
}
fn decimal<W: Write>(out: &mut W, mantissa: i64, scale: u8) -> io::Result<()> {
    if mantissa < 0 {
        out.write_all(b"-")?;
    }
    let digits = mantissa.unsigned_abs().to_string();
    let scale = scale as usize;
    if scale == 0 {
        return out.write_all(digits.as_bytes());
    }
    if digits.len() > scale {
        let split = digits.len() - scale;
        write!(out, "{}.{}", &digits[..split], &digits[split..])
    } else {
        out.write_all(b"0.")?;
        for _ in digits.len()..scale {
            out.write_all(b"0")?;
        }
        out.write_all(digits.as_bytes())
    }
}
fn cell<W: Write, F>(out: &mut W, f: F) -> Result<()>
where
    F: FnOnce(&mut Quoted<'_, W>) -> Result<()>,
{
    out.write_all(b"\"")?;
    f(&mut Quoted(out))?;
    out.write_all(b"\"")?;
    Ok(())
}
struct Quoted<'a, W>(&'a mut W);
impl<W: Write> Write for Quoted<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut start = 0;
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'"' {
                self.0.write_all(&bytes[start..i])?;
                self.0.write_all(b"\"\"")?;
                start = i + 1;
            }
        }
        self.0.write_all(&bytes[start..])?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
