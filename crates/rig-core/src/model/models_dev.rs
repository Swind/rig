//! Cached model metadata from models.dev, indexed by provider and model id.
//! Clones share one catalog and serialize the initial fetch. Successful catalogs
//! stay cached until invalidated; transport deadlines are set by the caller.
//!
//! ```no_run
//! use rig_core::{http_client::DynHttpClient, model::models_dev::ModelsDev};
//! async fn context(http: DynHttpClient) -> Result<Option<u32>, rig_core::model::models_dev::Error> {
//!     let catalog = ModelsDev::new(http);
//!     Ok(catalog.get("opencode-go", "deepseek-v4.1-flash").await?
//!         .and_then(|model| model.context_length))
//! }
//! ```

use super::ModelInfo;
use crate::http_client::{self, DynHttpClient, HttpClientExt, NoBody, Request, StatusCode, Uri};
use futures::{TryStreamExt, lock::Mutex};
use serde_json::Value;
use std::sync::Arc;

const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Failures fetching or decoding a models.dev catalog. Failed fetches are not cached.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Transport or HTTP request construction failed.
    #[error(transparent)]
    Http(#[from] http_client::Error),
    /// The server returned a non-success status.
    #[error("model catalog HTTP status {0}")]
    Status(StatusCode),
    /// The response exceeded the 16 MiB size limit.
    #[error("model catalog exceeds 16 MiB")]
    BodyLimit,
    /// The response was not valid JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The JSON root was not a provider object.
    #[error("model catalog must be a provider object")]
    InvalidCatalog,
}

/// A shared, in-memory models.dev catalog cache, independent of provider credentials.
/// Reuse this value or a clone across lookups. No disk storage or automatic expiry is used.
#[derive(Clone)]
pub struct ModelsDev {
    http: DynHttpClient,
    source: Uri,
    cache: Arc<Mutex<Option<Value>>>,
}

impl ModelsDev {
    /// Creates an empty cache using the supplied HTTP client and models.dev API.
    /// Set transport timeouts on the client. Do not attach provider authorization headers.
    pub fn new(http: DynHttpClient) -> Self {
        Self::with_source(http, Uri::from_static("https://models.dev/api.json"))
    }

    /// Creates an empty cache using a models.dev-format mirror at `source`.
    /// The source must be an HTTP(S) URI supported by the supplied client.
    pub fn with_source(http: DynHttpClient, source: Uri) -> Self {
        Self {
            http,
            source,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns model metadata, or `None` for an unknown provider/model pair.
    /// Missing, zero, negative, non-integer or oversized token limits are absent.
    /// Fetches once per cache generation; concurrent callers share the result.
    /// Returns transport, status, size or JSON errors without caching a failed fetch.
    pub async fn get(&self, provider: &str, model: &str) -> Result<Option<ModelInfo>, Error> {
        let mut cache = self.cache.lock().await;
        if cache.is_none() {
            let request = Request::builder()
                .uri(self.source.clone())
                .header(
                    "user-agent",
                    concat!("rig-model-catalog/", env!("CARGO_PKG_VERSION")),
                )
                .body(NoBody)
                .map_err(http_client::Error::from)?;
            let response = self.http.send_streaming(request).await?;
            if !response.status().is_success() {
                return Err(Error::Status(response.status()));
            }
            let mut stream = response.into_body();
            let mut bytes = Vec::new();
            while let Some(chunk) = stream.try_next().await? {
                if bytes.len().saturating_add(chunk.len()) > BODY_LIMIT {
                    return Err(Error::BodyLimit);
                }
                bytes.extend_from_slice(&chunk);
            }
            let catalog: Value = serde_json::from_slice(&bytes)?;
            if !catalog.is_object() {
                return Err(Error::InvalidCatalog);
            }
            *cache = Some(catalog);
        }
        Ok(cache
            .as_ref()
            .and_then(|catalog| lookup(catalog, provider, model)))
    }

    /// Clears the shared cache. The next lookup downloads a fresh catalog.
    /// Waits for any fetch already in progress, then invalidates its result.
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}

fn lookup(catalog: &Value, provider: &str, model: &str) -> Option<ModelInfo> {
    let entry = catalog.get(provider)?.get("models")?.get(model)?;
    if !entry.is_object() {
        return None;
    }
    let mut info = ModelInfo::from_id(model);
    info.name = entry.get("name").and_then(Value::as_str).map(str::to_owned);
    info.description = entry
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let limit = |name| {
        entry
            .get("limit")?
            .get(name)?
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
    };
    info.context_length = limit("context");
    info.max_output_tokens = limit("output");
    Some(info)
}

#[cfg(test)]
mod tests;
