//! Bounded HTTP requests and authenticated downloads from explicitly trusted hosts.
//!
//! ```
//! use rig_messaging_platforms::Http;
//! use std::time::Duration;
//! let http = Http::new(Duration::from_secs(10), 1024 * 1024)?;
//! # Ok::<(), rig_messaging_platforms::Error>(())
//! ```
use crate::Error;
use ::http::Method;
use bytes::{Bytes, BytesMut};
use rig_reqwest::reqwest::{Client, RequestBuilder, Url};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Shared connection pool with finite deadlines and disabled redirects.
#[derive(Clone)]
pub struct Http {
    client: Client,
    max_bytes: usize,
}
impl Http {
    /// Construct a client with a positive deadline and response byte limit.
    pub fn new(timeout: Duration, max_bytes: usize) -> Result<Self, Error> {
        if timeout.is_zero() || max_bytes == 0 {
            return Err(Error::Invalid("HTTP limits must be positive"));
        }
        let client = Client::builder()
            .timeout(timeout)
            .redirect(rig_reqwest::reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { client, max_bytes })
    }
    /// Build a request using this client's deadlines and connection pool.
    pub fn request(&self, method: Method, url: &str) -> RequestBuilder {
        self.client.request(method, url)
    }
    /// Execute a request, rejecting unsuccessful status and oversized bodies.
    pub async fn execute(&self, request: RequestBuilder) -> Result<Bytes, Error> {
        self.execute_limited(request, self.max_bytes).await
    }
    /// Execute a request with an additional per-operation byte limit.
    pub async fn execute_limited(
        &self,
        request: RequestBuilder,
        limit: usize,
    ) -> Result<Bytes, Error> {
        let (status, _, body) = Self::bounded_response(request, limit.min(self.max_bytes)).await?;
        if !status.is_success() {
            return Err(Error::Status(status));
        }
        Ok(body)
    }
    /// Read a bounded response including unsuccessful status and original headers.
    pub async fn response(
        &self,
        request: RequestBuilder,
    ) -> Result<(::http::StatusCode, ::http::HeaderMap, Bytes), Error> {
        Self::bounded_response(request, self.max_bytes).await
    }
    /// Execute and deserialize a bounded JSON response.
    pub async fn json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, Error> {
        Ok(serde_json::from_slice(&self.execute(request).await?)?)
    }
    /// Download over HTTPS from an exact trusted host, with no redirect or URL credentials.
    pub async fn download(
        &self,
        url: &str,
        trusted_hosts: &[&str],
        bearer: Option<&str>,
        limit: usize,
    ) -> Result<Bytes, Error> {
        Ok(self
            .download_response(url, trusted_hosts, bearer, limit)
            .await?
            .1)
    }
    /// Download trusted HTTPS content while retaining its response headers.
    pub async fn download_response(
        &self,
        url: &str,
        trusted_hosts: &[&str],
        bearer: Option<&str>,
        limit: usize,
    ) -> Result<(::http::HeaderMap, Bytes), Error> {
        let url = Url::parse(url).map_err(|_| Error::Invalid("attachment URL"))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some_and(|port| port != 443)
            || !url
                .host_str()
                .is_some_and(|host| trusted_hosts.contains(&host))
        {
            return Err(Error::Invalid("untrusted attachment URL"));
        }
        let mut request = self.request(Method::GET, url.as_str());
        if let Some(token) = bearer {
            request = request.bearer_auth(token);
        }
        let (status, headers, body) =
            Self::bounded_response(request, limit.min(self.max_bytes)).await?;
        if !status.is_success() {
            return Err(Error::Status(status));
        }
        Ok((headers, body))
    }
    async fn bounded_response(
        request: RequestBuilder,
        limit: usize,
    ) -> Result<(::http::StatusCode, ::http::HeaderMap, Bytes), Error> {
        let mut response = request.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(Error::TooLarge);
        }
        let mut body = BytesMut::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > limit.saturating_sub(body.len()) {
                return Err(Error::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok((status, headers, body.freeze()))
    }
}
#[cfg(test)]
mod tests;
