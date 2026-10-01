//! Model metadata returned by providers with model listing support.
//!
//! Use [`ModelList`] for provider responses and [`ModelInfo`] for each
//! advertised model entry. A provider with a model-listing wire returns it
//! from `models()`; call it through a [`Model`](crate::driver::Model).
//! [`models_dev::ModelsDev`] supplies cached external context and output limits
//! through a caller-supplied HTTP transport.
//!
//! ```
//! use rig_core::model::ModelInfo;
//!
//! let model = ModelInfo::from_id("example");
//! assert_eq!(model.display_name(), "example");
//! ```

pub mod listing;
pub mod models_dev;

pub use listing::{ModelInfo, ModelList};
