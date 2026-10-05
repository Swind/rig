//! Parameterized Cypher execution with JSON-projected rows. Backends retain their
//! Cypher dialect and may execute statements that modify data.
//!
//! ```
//! use rig_core::cypher::CypherRequest;
//! use serde_json::json;
//!
//! let request = CypherRequest::new(
//!     "MATCH (p:Person) WHERE p.name = $name RETURN p.name AS name",
//! ).param("name", json!("Alice"));
//! # let _ = request;
//! ```

use std::{collections::BTreeMap, error::Error, future::Future};

use serde_json::Value;

use crate::wasm_compat::{WasmCompatSend, WasmCompatSync};

#[cfg(not(target_family = "wasm"))]
type BoxedError = Box<dyn Error + Send + Sync + 'static>;
#[cfg(target_family = "wasm")]
type BoxedError = Box<dyn Error + 'static>;

/// A single Cypher statement with separately bound parameters.
///
/// The statement must use the backend's dialect. Parameters use their names
/// without the `$` prefix. Project JSON-compatible values with unique column
/// aliases rather than returning nodes, relationships, or paths directly.
#[derive(Debug, Clone, PartialEq)]
pub struct CypherRequest {
    /// The Cypher statement to execute, which may modify data.
    pub statement: String,
    /// Named JSON values to bind without interpolating them into the statement.
    pub parameters: BTreeMap<String, Value>,
}

impl CypherRequest {
    /// Creates a request with no parameters. Validation is performed by the backend.
    pub fn new(statement: impl Into<String>) -> Self {
        Self {
            statement: statement.into(),
            parameters: BTreeMap::new(),
        }
    }

    /// Binds a parameter by name, replacing any existing value for that name.
    pub fn param(mut self, name: impl Into<String>, value: Value) -> Self {
        self.parameters.insert(name.into(), value);
        self
    }
}

/// One projected row keyed by its unique column aliases.
pub type CypherRow = BTreeMap<String, Value>;

/// Executes parameterized Cypher and returns owned, JSON-projected rows.
///
/// Backend implementations must reject unsupported parameter and result values
/// rather than substituting null or discarding columns. Statements are not
/// restricted to reads. Transaction behavior follows the backend.
pub trait CypherQuery: WasmCompatSend + WasmCompatSync {
    /// Executes one statement and preserves the backend's row order.
    ///
    /// Returns an empty vector when no rows are produced. Use `ORDER BY` when
    /// deterministic ordering is required. Errors identify parameter conversion,
    /// execution, or result conversion failures and retain their original source.
    fn query(
        &self,
        request: CypherRequest,
    ) -> impl Future<Output = Result<Vec<CypherRow>, CypherQueryError>> + WasmCompatSend;
}

/// Errors from Cypher parameter conversion, execution, or result conversion.
#[derive(Debug, thiserror::Error)]
pub enum CypherQueryError {
    /// A named parameter could not be represented by the backend.
    #[error("Cypher parameter `{name}` conversion failed: {source}")]
    Parameter {
        /// The parameter name without its `$` prefix.
        name: String,
        /// The original conversion error.
        source: BoxedError,
    },
    /// The backend could not execute or finish the statement.
    #[error("Cypher execution failed: {source}")]
    Query {
        /// The original backend error.
        source: BoxedError,
    },
    /// A result could not be represented as JSON-projected rows.
    #[error("Cypher result conversion failed: {source}")]
    Result {
        /// The original conversion error.
        source: BoxedError,
    },
}

impl CypherQueryError {
    /// Wraps a conversion error with the name of the unsupported parameter.
    pub fn parameter(
        name: impl Into<String>,
        source: impl Error + WasmCompatSend + WasmCompatSync + 'static,
    ) -> Self {
        Self::Parameter {
            name: name.into(),
            source: Box::new(source),
        }
    }

    /// Wraps a backend execution error.
    pub fn query(source: impl Error + WasmCompatSend + WasmCompatSync + 'static) -> Self {
        Self::Query {
            source: Box::new(source),
        }
    }

    /// Wraps a projected-result conversion error.
    pub fn result(source: impl Error + WasmCompatSend + WasmCompatSync + 'static) -> Self {
        Self::Result {
            source: Box::new(source),
        }
    }
}

#[cfg(test)]
mod tests;
