//! Output adapters over caller-supplied records. They neither grant access nor persist views.
use canonical_core::{Error, Result};
use canonical_registry::{Record, Registry, SchemaRef, Value};
use std::io::{self, Write};
mod cbr;
mod csv;
mod json;
pub use cbr::CbrReader;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsvNestedPolicy {
    Reject,
    JsonCell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv {
        schema: SchemaRef,
        nested: CsvNestedPolicy,
    },
    Cbr,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_records: u64,
    pub max_output_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_records: 1_000_000,
            max_output_bytes: 1024 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WriteStats {
    pub records: u64,
    pub bytes: u64,
}

pub struct Reader<'a> {
    registry: &'a Registry,
    limits: Limits,
}
impl<'a> Reader<'a> {
    pub fn new(registry: &'a Registry, limits: Limits) -> Result<Self> {
        if limits.max_records == 0 || limits.max_output_bytes == 0 {
            return Err(Error::InvalidInput("view limits must be positive"));
        }
        Ok(Self { registry, limits })
    }
    pub fn write_one<W: Write>(
        &self,
        format: Format,
        record: Option<&Record>,
        output: &mut W,
    ) -> Result<WriteStats> {
        let mut out = Output::new(output, self.limits.max_output_bytes);
        let mut count = 0;
        let result = (|| {
            self.start(format, false, &mut out)?;
            if let Some(record) = record {
                self.write_record(format, record, 1, &mut out)?;
                count = 1;
            } else if format == Format::Json {
                out.write_all(b"null")?;
            }
            Ok(())
        })();
        out.finish(result, count)
    }
    pub fn write_many<I, W>(&self, format: Format, records: I, output: &mut W) -> Result<WriteStats>
    where
        I: IntoIterator<Item = Result<Record>>,
        W: Write,
    {
        let mut out = Output::new(output, self.limits.max_output_bytes);
        let mut count = 0u64;
        let result = (|| {
            self.start(format, true, &mut out)?;
            for item in records {
                if count >= self.limits.max_records {
                    return Err(Error::TooLarge);
                }
                let record = item?;
                if format == Format::Json && count > 0 {
                    out.write_all(b",")?;
                }
                self.write_record(format, &record, count + 1, &mut out)?;
                count += 1;
            }
            if format == Format::Json {
                out.write_all(b"]")?;
            }
            Ok(())
        })();
        out.finish(result, count)
    }
    fn start<W: Write>(&self, format: Format, collection: bool, out: &mut W) -> Result<()> {
        match format {
            Format::Json if collection => out.write_all(b"[")?,
            Format::Csv { schema, .. } => csv::header(self.registry.schema(schema)?, out)?,
            _ => (),
        };
        Ok(())
    }
    fn write_record<W: Write>(
        &self,
        format: Format,
        record: &Record,
        sequence: u64,
        out: &mut W,
    ) -> Result<()> {
        let values = self.validate(record)?;
        match format {
            Format::Json => json::write(
                out,
                &json::JsonRecord {
                    registry: self.registry,
                    record,
                    values: &values,
                },
            ),
            Format::Csv { schema, nested } => {
                if record.schema != schema {
                    return Err(Error::InvalidInput(
                        "CSV collection contains another schema",
                    ));
                }
                csv::record(self.registry, record, &values, nested, out)
            }
            Format::Cbr => cbr::write(record, sequence, out),
        }
    }
    fn validate(&self, record: &Record) -> Result<Vec<Option<Value>>> {
        if record.id == 0 || record.revision == 0 {
            return Err(Error::InvalidInput(
                "record identity/revision must be positive",
            ));
        }
        if self.registry.namespace(record.namespace).is_none() {
            return Err(Error::InvalidInput("unknown record namespace"));
        }
        self.registry.decode_values(record.schema, &record.payload)
    }
}

struct Output<'a, W> {
    inner: &'a mut W,
    limit: u64,
    bytes: u64,
    exceeded: bool,
}
impl<'a, W: Write> Output<'a, W> {
    fn new(inner: &'a mut W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            bytes: 0,
            exceeded: false,
        }
    }
    fn finish(self, result: Result<()>, records: u64) -> Result<WriteStats> {
        if self.exceeded {
            return Err(Error::TooLarge);
        }
        result.map(|()| WriteStats {
            records,
            bytes: self.bytes,
        })
    }
}
impl<W: Write> Write for Output<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit - self.bytes {
            self.exceeded = true;
            return Err(io::Error::other("view output limit reached"));
        }
        let count = self.inner.write(bytes)?;
        self.bytes += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
