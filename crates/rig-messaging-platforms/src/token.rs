//! Serialized access-token refresh with an expiry safety margin.
//!
//! ```
//! use rig_messaging_platforms::TokenCache;
//! let cache = TokenCache::new();
//! ```
use crate::Error;
use std::{future::Future, time::Duration};
use tokio::{sync::Mutex, time::Instant};

/// A shared token cache. Failed refreshes are never cached.
#[derive(Default)]
pub struct TokenCache {
    token: Mutex<Option<(String, Instant)>>,
}
impl TokenCache {
    /// Construct an empty cache.
    pub fn new() -> Self {
        Self::default()
    }
    /// Return a fresh token, serializing refresh and reserving up to 30 seconds of lifetime.
    pub async fn get_or_refresh<F, Fut>(&self, refresh: F) -> Result<String, Error>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(String, Duration), Error>>,
    {
        let mut cached = self.token.lock().await;
        if let Some((token, expires)) = cached.as_ref()
            && *expires > Instant::now()
        {
            return Ok(token.clone());
        }
        let (token, lifetime) = refresh().await?;
        if token.is_empty() || lifetime.is_zero() {
            return Err(Error::Invalid("empty or expired access token"));
        }
        let margin = Duration::from_secs(30).min(lifetime / 10);
        let expires = Instant::now()
            .checked_add(lifetime - margin)
            .ok_or(Error::Invalid("access token lifetime"))?;
        *cached = Some((token.clone(), expires));
        Ok(token)
    }
    /// Forget a rejected token before one authorized refresh attempt.
    pub async fn invalidate(&self) {
        *self.token.lock().await = None;
    }
}
#[cfg(test)]
mod tests;
