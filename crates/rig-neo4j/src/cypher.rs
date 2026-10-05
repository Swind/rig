use std::{collections::BTreeMap, error::Error, fmt};

use neo4rs::{BoltMap, BoltNull, BoltString, BoltType, Row};
use rig_core::cypher::{CypherQuery, CypherQueryError, CypherRequest, CypherRow};
use serde::{
    Deserialize, Deserializer,
    de::{EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor},
};
use serde_json::Value;

use crate::Neo4jClient;

impl CypherQuery for Neo4jClient {
    async fn query(&self, request: CypherRequest) -> Result<Vec<CypherRow>, CypherQueryError> {
        let mut query = neo4rs::query(&request.statement);
        for (name, value) in request.parameters {
            let value =
                to_bolt(value).map_err(|error| CypherQueryError::parameter(name.clone(), error))?;
            query = query.param(&name, value);
        }

        let mut stream = self
            .graph
            .execute(query)
            .await
            .map_err(CypherQueryError::query)?;
        let mut rows = Vec::new();
        while let Some(row) = stream.next().await.map_err(CypherQueryError::query)? {
            rows.push(to_row(row)?);
        }
        Ok(rows)
    }
}

#[derive(Debug)]
enum ConversionError {
    IntegerOutOfRange,
    NonFiniteFloat,
}

impl fmt::Display for ConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntegerOutOfRange => {
                formatter.write_str("integer exceeds Bolt's signed 64-bit range")
            }
            Self::NonFiniteFloat => {
                formatter.write_str("non-finite float cannot be represented as JSON")
            }
        }
    }
}

impl Error for ConversionError {}

fn to_bolt(value: Value) -> Result<BoltType, ConversionError> {
    Ok(match value {
        Value::Null => BoltType::Null(BoltNull),
        Value::Bool(value) => value.into(),
        Value::String(value) => value.into(),
        Value::Number(value) => {
            if value.is_i64() || value.is_u64() {
                value
                    .as_i64()
                    .ok_or(ConversionError::IntegerOutOfRange)?
                    .into()
            } else {
                value
                    .as_f64()
                    .ok_or(ConversionError::NonFiniteFloat)?
                    .into()
            }
        }
        Value::Array(values) => BoltType::List(
            values
                .into_iter()
                .map(to_bolt)
                .collect::<Result<Vec<_>, _>>()?
                .into(),
        ),
        Value::Object(values) => {
            let mut map = BoltMap::new();
            for (key, value) in values {
                map.put(BoltString::new(&key), to_bolt(value)?);
            }
            BoltType::Map(map)
        }
    })
}

fn to_row(row: Row) -> Result<CypherRow, CypherQueryError> {
    let columns = row
        .to_strict::<BTreeMap<String, JsonProjection>>()
        .map_err(CypherQueryError::result)?;
    Ok(columns
        .into_iter()
        .map(|(name, value)| (name, value.0))
        .collect())
}

struct JsonProjection(Value);

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum JsonKind {
    String,
    Boolean,
    Map,
    Null,
    Integer,
    Float,
    List,
}

impl<'de> Deserialize<'de> for JsonProjection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // neo4rs dispatches native type tags only for this enum name. Its own
        // BoltType deserializer turns durations into lists, so validate tags first.
        deserializer.deserialize_enum(std::any::type_name::<BoltType>(), &[], ProjectionVisitor)
    }
}

struct ProjectionVisitor;

impl<'de> Visitor<'de> for ProjectionVisitor {
    type Value = JsonProjection;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON-compatible Cypher projection")
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        let (_kind, variant): (JsonKind, _) = data.variant()?;
        variant.tuple_variant(1, self)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(JsonProjection(Value::Null))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(JsonProjection(Value::Bool(value)))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(JsonProjection(Value::String(value.into())))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(JsonProjection(Value::Number(value.into())))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        serde_json::Number::from_f64(value)
            .map(|value| JsonProjection(Value::Number(value)))
            .ok_or_else(|| E::custom(ConversionError::NonFiniteFloat))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
        let mut result = Vec::new();
        while let Some(JsonProjection(value)) = values.next_element()? {
            result.push(value);
        }
        Ok(JsonProjection(Value::Array(result)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut values: A) -> Result<Self::Value, A::Error> {
        let mut result = serde_json::Map::new();
        while let Some((key, JsonProjection(value))) = values.next_entry()? {
            result.insert(key, value);
        }
        Ok(JsonProjection(Value::Object(result)))
    }
}

#[cfg(test)]
mod tests;
