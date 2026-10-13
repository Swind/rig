//! Cached models.dev data layered over Rig's built-in model catalog.
//! Clones share one catalog and serialize the initial fetch. Successful catalogs
//! stay cached until invalidated; transport deadlines are set by the caller.
//!
//! ```no_run
//! use rig_core::{http_client::DynHttpClient, catalog::models_dev::ModelsDev};
//! async fn context(http: DynHttpClient) -> Result<Option<u32>, Box<dyn std::error::Error>> {
//!     let catalog = ModelsDev::new(http).catalog().await?;
//!     Ok(catalog.resolve("opencode-go/deepseek-v4.1-flash")?.spec.context_window)
//! }
//! ```

use super::{Catalog, CatalogError, Skipped};
use crate::http_client::{self, DynHttpClient, HttpClientExt, NoBody, Request, StatusCode, Uri};
use futures::{TryStreamExt, lock::Mutex};
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
    /// The response was not a models.dev provider object.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The response was not UTF-8.
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
}

/// A shared, in-memory models.dev catalog cache, independent of provider credentials.
/// Reuse this value or a clone across lookups. No disk storage or automatic expiry is used.
#[derive(Clone)]
pub struct ModelsDev {
    http: DynHttpClient,
    source: Uri,
    cache: Arc<Mutex<Option<(Catalog, Vec<Skipped>)>>>,
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

    /// Returns a catalog with fetched data layered over the built-in facts.
    /// Hand-reviewed fields remain pinned. Unknown vendors and invalid rows are skipped.
    /// Fetches once per cache generation; concurrent callers share the result.
    /// Returns transport, status, size or JSON errors without caching a failed fetch.
    pub async fn catalog(&self) -> Result<Catalog, Error> {
        self.load().await.map(|(catalog, _)| catalog)
    }

    /// Returns the merged catalog and diagnostics for skipped vendors or rows.
    /// Has the same fetch, caching and error behavior as [`Self::catalog`].
    pub async fn load(&self) -> Result<(Catalog, Vec<Skipped>), Error> {
        let mut cache = self.cache.lock().await;
        if let Some(result) = cache.as_ref() {
            return Ok(result.clone());
        }
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
        let (catalog, skipped) = Catalog::from_models_dev(std::str::from_utf8(&bytes)?)?;
        let result = (Catalog::builtin().with_models_dev(&catalog), skipped);
        *cache = Some(result.clone());
        Ok(result)
    }

    /// Clears the shared cache. The next lookup downloads a fresh catalog.
    /// Waits for any fetch already in progress, then invalidates its result.
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
}

#[cfg(test)]
mod tests;
