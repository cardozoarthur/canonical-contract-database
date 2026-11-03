//! Independent E15 B-tree oracle. Never compiled into production or profiling binaries.
use super::*;
#[derive(Default)]
pub(super) struct LegacyIndex {
    rows: BTreeMap<RowKey, Row>,
    secondary: BTreeMap<FieldRef, BTreeMap<Scalar, KeySet>>,
    by_namespace: BTreeMap<u64, KeySet>,
    by_schema: BTreeMap<SchemaRef, KeySet>,
}
impl LegacyIndex {
    pub(super) fn new(registry: &Registry, snapshot: &Snapshot) -> Result<Self> {
        let mut index = Self::default();
        for record in snapshot.iter() {
            let schema = registry.schema(record.schema)?;
            let values = registry.decode_values(record.schema, &record.payload)?;
            let required_namespaces = if let Some(rule) = registry.relation_rule(record.schema) {
                let [
                    Some(Value::Relation {
                        source,
                        kind,
                        target,
                    }),
                ] = values.as_slice()
                else {
                    return Err(invalid("legacy native payload"));
                };
                if *kind != rule.kind
                    || snapshot
                        .record(source.namespace, source.id)
                        .map(|r| r.schema)
                        != Some(rule.source_schema)
                    || snapshot
                        .record(target.namespace, target.id)
                        .map(|r| r.schema)
                        != Some(rule.target_schema)
                {
                    return Err(invalid("legacy native endpoints"));
                }
                [record.namespace, source.namespace, target.namespace]
            } else {
                [record.namespace; 3]
            };
            let key = (record.namespace, record.id, record.revision);
            let mut scalars = Vec::with_capacity(values.len());
            for (position, (field, value)) in schema.fields.iter().zip(values).enumerate() {
                let value = if supported(&field.kind) {
                    value.as_ref().map(|v| scalar(&field.kind, v)).transpose()?
                } else {
                    None
                };
                if let Some(value) = &value {
                    index
                        .secondary
                        .entry(FieldRef {
                            schema: record.schema,
                            position,
                        })
                        .or_default()
                        .entry(value.clone())
                        .or_default()
                        .insert(key);
                }
                scalars.push(value);
            }
            index
                .by_namespace
                .entry(record.namespace)
                .or_default()
                .insert(key);
            index
                .by_schema
                .entry(record.schema)
                .or_default()
                .insert(key);
            index.rows.insert(
                key,
                Row {
                    schema: record.schema,
                    values: scalars,
                    required_namespaces,
                },
            );
        }
        Ok(index)
    }
    fn candidates(
        &self,
        engine: &QueryEngine,
        predicate: &Predicate,
    ) -> Result<(Option<KeySet>, u64)> {
        match predicate {
            Predicate::True => Ok((None, 0)),
            Predicate::Eq(field, value) => {
                let value = scalar(engine.field_kind(*field)?, value)?;
                Ok((
                    Some(
                        self.secondary
                            .get(field)
                            .and_then(|index| index.get(&value))
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    1,
                ))
            }
            Predicate::Range {
                field,
                lower,
                upper,
            } => {
                use std::ops::Bound::{Included, Unbounded};
                let kind = engine.field_kind(*field)?;
                let lo = lower.as_ref().map(|v| scalar(kind, v)).transpose()?;
                let hi = upper.as_ref().map(|v| scalar(kind, v)).transpose()?;
                let lower = lo.as_ref().map_or(Unbounded, Included);
                let upper = hi.as_ref().map_or(Unbounded, Included);
                let keys = self
                    .secondary
                    .get(field)
                    .map(|index| {
                        index
                            .range((lower, upper))
                            .flat_map(|(_, set)| set.iter().copied())
                            .collect()
                    })
                    .unwrap_or_default();
                Ok((Some(keys), 1))
            }
            Predicate::And(children) => {
                // Equality postings provide exact cardinalities without copying large ranges.
                // Seed with the most selective conjunct and intersect equality sets by membership;
                // range/OR residuals are evaluated once on authorized candidate rows by query().
                let mut equality = Vec::new();
                let mut probes = 0;
                self.conjunctive_equalities(engine, predicate, &mut equality, &mut probes)?;
                if !equality.is_empty() {
                    if equality.iter().any(Option::is_none) {
                        return Ok((Some(KeySet::new()), probes));
                    }
                    equality.sort_by_key(|set| set.map_or(0, BTreeSet::len));
                    let mut keys = equality[0].expect("checked posting").clone();
                    for set in equality.into_iter().skip(1) {
                        keys.retain(|key| set.expect("checked posting").contains(key));
                    }
                    return Ok((Some(keys), probes));
                }
                // Without equality, use one available bounded index range as a safe superset.
                // Other conjuncts remain residual filters, avoiding materializing every posting set.
                for child in children {
                    let (set, n) = self.candidates(engine, child)?;
                    probes += n;
                    if let Some(set) = set {
                        return Ok((Some(set), probes));
                    }
                }
                Ok((None, probes))
            }
            Predicate::Or(children) => {
                let mut keys = KeySet::new();
                let mut probes = 0;
                let mut all = false;
                for child in children {
                    let (set, n) = self.candidates(engine, child)?;
                    probes += n;
                    if let Some(set) = set {
                        keys.extend(set);
                    } else {
                        all = true;
                    }
                }
                Ok((if all { None } else { Some(keys) }, probes))
            }
        }
    }
    fn conjunctive_equalities<'a>(
        &'a self,
        engine: &QueryEngine,
        predicate: &Predicate,
        sets: &mut Vec<Option<&'a KeySet>>,
        probes: &mut u64,
    ) -> Result<()> {
        match predicate {
            Predicate::Eq(field, value) => {
                let value = scalar(engine.field_kind(*field)?, value)?;
                *probes += 1;
                sets.push(
                    self.secondary
                        .get(field)
                        .and_then(|index| index.get(&value)),
                );
            }
            Predicate::And(children) => {
                for child in children {
                    self.conjunctive_equalities(engine, child, sets, probes)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    pub(super) fn selection_keys(
        &self,
        engine: &QueryEngine,
        subject: &Subject,
        query: &Query,
    ) -> Result<(Vec<RowKey>, usize, u64, bool)> {
        let (selected, mut index_lookups) = self.candidates(engine, &query.filter)?;
        let indexed = selected.is_some() || !query.schemas.is_empty();
        let selected = if query.schemas.is_empty() {
            selected
        } else {
            let schema_sets: Vec<_> = query
                .schemas
                .iter()
                .filter_map(|schema| self.by_schema.get(schema))
                .collect();
            index_lookups += query.schemas.len() as u64;
            Some(match selected {
                Some(mut set) => {
                    // Do not copy an entire schema's postings for a selective predicate.
                    set.retain(|key| schema_sets.iter().any(|schema| schema.contains(key)));
                    set
                }
                None => schema_sets
                    .into_iter()
                    .flat_map(|set| set.iter().copied())
                    .collect(),
            })
        };
        let mut keys = selected.unwrap_or_else(|| {
            query
                .namespaces
                .iter()
                .filter_map(|ns| self.by_namespace.get(ns))
                .flat_map(|set| set.iter().copied())
                .collect()
        });
        keys.retain(|key| {
            query.namespaces.contains(&key.0)
                && self.rows.get(key).is_some_and(|row| {
                    row.required_namespaces
                        .iter()
                        .all(|ns| subject.readable_namespaces.contains(ns))
                        && (query.schemas.is_empty() || query.schemas.contains(&row.schema))
                })
        });
        let examined = keys.len();
        let matches = keys
            .into_iter()
            .filter(|key| engine.matches(&self.rows[key], &query.filter))
            .collect::<Vec<_>>();
        Ok((matches, examined, index_lookups, indexed))
    }
    pub(super) fn compare(
        &self,
        engine: &QueryEngine,
        a: RowKey,
        b: RowKey,
        order: &Order,
    ) -> Ordering {
        if order.field.is_none() {
            return if order.descending {
                b.cmp(&a)
            } else {
                a.cmp(&b)
            };
        }
        let first = order
            .field
            .map(|field| {
                engine
                    .value(&self.rows[&a], field)
                    .cmp(&engine.value(&self.rows[&b], field))
            })
            .unwrap_or(Ordering::Equal);
        let first = if order.descending {
            first.reverse()
        } else {
            first
        };
        first.then_with(|| a.cmp(&b))
    }
}
