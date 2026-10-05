use std::{collections::BTreeSet, path::Path, sync::Arc};

use lbug::{Connection, Database, LogicalType, SystemConfig, Value as NativeValue};
use rig_core::cypher::{CypherQuery, CypherQueryError, CypherRequest, CypherRow};
use serde_json::{Number, Value};

/// A shared native database handle. Clones open connections to the same database.
#[derive(Clone)]
pub struct LadybugClient {
    database: Arc<Database>,
}

impl LadybugClient {
    /// Opens or creates a native database at `path`.
    ///
    /// Opening is synchronous. Call from a blocking thread when startup latency
    /// must not occupy an async executor thread. Returns the native open error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, lbug::Error> {
        Self::open_with_config(path, SystemConfig::default())
    }

    /// Opens or creates a database with native memory and execution settings.
    ///
    /// Returns the native open error. This operation is synchronous.
    pub fn open_with_config(
        path: impl AsRef<Path>,
        config: SystemConfig,
    ) -> Result<Self, lbug::Error> {
        Ok(Self {
            database: Arc::new(Database::new(path, config)?),
        })
    }

    /// Executes requests on one connection in a write transaction.
    ///
    /// Commits only after every request and result conversion succeeds. On failure
    /// rolls back and returns the original error. Dropping the future does not
    /// cancel an already running native transaction. Requests must not contain
    /// transaction-control statements.
    pub async fn transaction(
        &self,
        requests: Vec<CypherRequest>,
    ) -> Result<Vec<Vec<CypherRow>>, CypherQueryError> {
        let database = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let connection = Connection::new(&database).map_err(CypherQueryError::query)?;
            connection
                .query("BEGIN TRANSACTION")
                .map_err(CypherQueryError::query)?;
            let outcome = requests
                .into_iter()
                .map(|request| execute(&connection, request))
                .collect();
            match outcome {
                Ok(rows) => {
                    connection
                        .query("COMMIT")
                        .map_err(CypherQueryError::query)?;
                    Ok(rows)
                }
                Err(error) => {
                    let _ = connection.query("ROLLBACK");
                    Err(error)
                }
            }
        })
        .await
        .map_err(CypherQueryError::query)?
    }
}

impl CypherQuery for LadybugClient {
    async fn query(&self, request: CypherRequest) -> Result<Vec<CypherRow>, CypherQueryError> {
        let database = self.database.clone();
        tokio::task::spawn_blocking(move || {
            let connection = Connection::new(&database).map_err(CypherQueryError::query)?;
            execute(&connection, request)
        })
        .await
        .map_err(CypherQueryError::query)?
    }
}

fn execute(
    connection: &Connection<'_>,
    request: CypherRequest,
) -> Result<Vec<CypherRow>, CypherQueryError> {
    let parameters = request
        .parameters
        .iter()
        .map(|(name, value)| {
            to_native(value)
                .map(|value| (name.as_str(), value))
                .map_err(|error| CypherQueryError::parameter(name, error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut statement = connection
        .prepare(&request.statement)
        .map_err(CypherQueryError::query)?;
    let result = connection
        .execute(&mut statement, parameters)
        .map_err(CypherQueryError::query)?;
    let names = result.get_column_names();
    if names.iter().collect::<BTreeSet<_>>().len() != names.len() {
        return Err(CypherQueryError::result(ProjectionError::DuplicateColumn));
    }
    result
        .map(|values| {
            if values.len() != names.len() {
                return Err(CypherQueryError::result(ProjectionError::ColumnCount));
            }
            names
                .iter()
                .cloned()
                .zip(values)
                .map(|(name, value)| {
                    to_json(value)
                        .map(|value| (name, value))
                        .map_err(CypherQueryError::result)
                })
                .collect()
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum ProjectionError {
    #[error("integer exceeds signed 64-bit JSON projection range")]
    IntegerRange,
    #[error("non-finite floating point value")]
    NonFinite,
    #[error("Ladybug lists require one element type")]
    ListType,
    #[error("unsupported native projection type: {0:?}")]
    Unsupported(LogicalType),
    #[error("map keys must be strings and unique")]
    MapKey,
    #[error("column aliases must be unique")]
    DuplicateColumn,
    #[error("result column count does not match its aliases")]
    ColumnCount,
}

fn to_native(value: &Value) -> Result<NativeValue, ProjectionError> {
    Ok(match value {
        Value::Null => NativeValue::Null(LogicalType::Any),
        Value::Bool(value) => NativeValue::Bool(*value),
        Value::String(value) => NativeValue::String(value.clone()),
        Value::Number(value) if value.is_i64() || value.is_u64() => {
            NativeValue::Int64(value.as_i64().ok_or(ProjectionError::IntegerRange)?)
        }
        Value::Number(value) => {
            NativeValue::Double(value.as_f64().ok_or(ProjectionError::NonFinite)?)
        }
        Value::Object(value) => NativeValue::Struct(
            value
                .iter()
                .map(|(name, value)| to_native(value).map(|value| (name.clone(), value)))
                .collect::<Result<_, _>>()?,
        ),
        Value::Array(value) => {
            let mut values = value.iter().map(to_native).collect::<Result<Vec<_>, _>>()?;
            let element_type = values
                .iter()
                .find(|value| !matches!(value, NativeValue::Null(_)))
                .map(LogicalType::from)
                .unwrap_or(LogicalType::Int64);
            for value in &mut values {
                if matches!(value, NativeValue::Null(_)) {
                    *value = NativeValue::Null(element_type.clone());
                } else if LogicalType::from(&*value) != element_type {
                    return Err(ProjectionError::ListType);
                }
            }
            NativeValue::List(element_type, values)
        }
    })
}

fn to_json(value: NativeValue) -> Result<Value, ProjectionError> {
    Ok(match value {
        NativeValue::Null(_) => Value::Null,
        NativeValue::Bool(value) => Value::Bool(value),
        NativeValue::String(value) => Value::String(value),
        NativeValue::Int64(value) => value.into(),
        NativeValue::Int32(value) => value.into(),
        NativeValue::Int16(value) => value.into(),
        NativeValue::Int8(value) => value.into(),
        NativeValue::UInt8(value) => value.into(),
        NativeValue::UInt16(value) => value.into(),
        NativeValue::UInt32(value) => value.into(),
        NativeValue::UInt64(value) => i64::try_from(value)
            .map_err(|_| ProjectionError::IntegerRange)?
            .into(),
        NativeValue::Int128(value) => i64::try_from(value)
            .map_err(|_| ProjectionError::IntegerRange)?
            .into(),
        NativeValue::Double(value) => {
            Value::Number(Number::from_f64(value).ok_or(ProjectionError::NonFinite)?)
        }
        NativeValue::Float(value) => {
            Value::Number(Number::from_f64(value.into()).ok_or(ProjectionError::NonFinite)?)
        }
        NativeValue::List(_, values) | NativeValue::Array(_, values) => {
            Value::Array(values.into_iter().map(to_json).collect::<Result<_, _>>()?)
        }
        NativeValue::Struct(values) => {
            let mut object = serde_json::Map::new();
            for (key, value) in values {
                if object.insert(key, to_json(value)?).is_some() {
                    return Err(ProjectionError::MapKey);
                }
            }
            Value::Object(object)
        }
        NativeValue::Map(_, values) => {
            let mut object = serde_json::Map::new();
            for (key, value) in values {
                let NativeValue::String(key) = key else {
                    return Err(ProjectionError::MapKey);
                };
                if object.insert(key, to_json(value)?).is_some() {
                    return Err(ProjectionError::MapKey);
                }
            }
            Value::Object(object)
        }
        value => return Err(ProjectionError::Unsupported(LogicalType::from(&value))),
    })
}

#[cfg(test)]
mod tests;
