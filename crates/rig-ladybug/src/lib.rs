//! Embedded Ladybug Cypher execution with owned JSON-projected rows. Native
//! operations run on Tokio's blocking pool and require a Tokio runtime.
//!
//! ```no_run
//! use rig_core::cypher::{CypherQuery, CypherRequest};
//! use rig_ladybug::LadybugClient;
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = LadybugClient::open("conversation.graph")?;
//! let rows = client.query(CypherRequest::new("RETURN 42 AS answer")).await?;
//! # let _ = rows;
//! # Ok(()) }
//! ```

#[cfg(target_family = "wasm")]
compile_error!("rig-ladybug is a native-only graph database backend");

#[cfg(not(target_family = "wasm"))]
mod native;

#[cfg(not(target_family = "wasm"))]
pub use native::LadybugClient;

#[cfg(not(target_family = "wasm"))]
pub use lbug::{Error, SystemConfig};
