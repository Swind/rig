//! Raw-body signature verification and pinned JWT key validation.
//! ```
//! use rig_messaging_platforms::auth::verify_secret;
//! assert!(verify_secret(b"configured", b"configured").is_ok());
//! ```
use crate::Error;
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Verify decoded HMAC-SHA256 signature bytes in constant time.
pub fn verify_hmac_sha256(secret: &[u8], body: &[u8], signature: &[u8]) -> Result<(), Error> {
    if secret.is_empty() {
        return Err(Error::Authentication);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| Error::Authentication)?;
    mac.update(body);
    mac.verify_slice(signature)
        .map_err(|_| Error::Authentication)
}

/// Compare secret bytes without data-dependent early exit for equal lengths.
pub fn verify_secret(expected: &[u8], supplied: &[u8]) -> Result<(), Error> {
    if expected.is_empty() {
        return Err(Error::Authentication);
    }
    verify_hmac_sha256(expected, supplied, &{
        let mut mac =
            Hmac::<Sha256>::new_from_slice(expected).map_err(|_| Error::Authentication)?;
        mac.update(expected);
        mac.finalize().into_bytes()
    })
}
#[cfg(test)]
mod tests;

#[cfg(any(feature = "teams", feature = "googlechat"))]
mod jwt {
    use super::*;
    use crate::Http;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
    use serde::de::DeserializeOwned;
    use std::time::Duration;
    use tokio::{sync::Mutex, time::Instant};

    /// JWT verifier with pinned issuer, audience, and HTTPS JWKS source.
    pub struct JwtVerifier {
        http: Http,
        source: String,
        validation: Validation,
        keys: Mutex<Option<(JwkSet, serde_json::Value, Instant)>>,
    }
    impl JwtVerifier {
        /// Construct an RS256 verifier. Keys are never fetched from token headers.
        pub fn new(
            http: Http,
            source: String,
            issuer: &str,
            audience: &str,
        ) -> Result<Self, Error> {
            let url = rig_reqwest::reqwest::Url::parse(&source)
                .map_err(|_| Error::Invalid("JWKS URL"))?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || issuer.is_empty()
                || audience.is_empty()
            {
                return Err(Error::Invalid("JWT configuration"));
            }
            let mut validation = Validation::new(Algorithm::RS256);
            validation.set_issuer(&[issuer]);
            validation.set_audience(&[audience]);
            validation.set_required_spec_claims(&["exp", "iss", "aud"]);
            validation.validate_nbf = true;
            validation.leeway = 30;
            Ok(Self {
                http,
                source,
                validation,
                keys: Mutex::new(None),
            })
        }
        /// Set a finite list of trusted issuer aliases for the configured provider.
        pub fn with_issuers(mut self, issuers: &[&str]) -> Result<Self, Error> {
            if issuers.is_empty() || issuers.iter().any(|issuer| issuer.is_empty()) {
                return Err(Error::Invalid("JWT issuers"));
            }
            self.validation.set_issuer(issuers);
            Ok(self)
        }
        #[cfg(test)]
        pub(crate) async fn seed_keys(&self, raw: serde_json::Value) -> Result<(), Error> {
            let jwks: JwkSet = serde_json::from_value(raw.clone())?;
            *self.keys.lock().await = Some((jwks, raw, Instant::now() + Duration::from_secs(300)));
            Ok(())
        }
        /// Validate signature and required claims using cached keys from the configured source.
        pub async fn verify<T: DeserializeOwned>(&self, token: &str) -> Result<T, Error> {
            self.verify_inner(token, None).await
        }
        /// Require a Bot Connector key endorsement for the supplied channel.
        pub async fn verify_endorsed<T: DeserializeOwned>(
            &self,
            token: &str,
            channel: &str,
        ) -> Result<T, Error> {
            self.verify_inner(token, Some(channel)).await
        }
        async fn verify_inner<T: DeserializeOwned>(
            &self,
            token: &str,
            channel: Option<&str>,
        ) -> Result<T, Error> {
            let header = decode_header(token).map_err(|_| Error::Authentication)?;
            if header.alg != Algorithm::RS256 {
                return Err(Error::Authentication);
            }
            let kid = header.kid.ok_or(Error::Authentication)?;
            let mut keys = self.keys.lock().await;
            if keys.as_ref().is_none_or(|(jwks, _, expires)| {
                *expires <= Instant::now()
                    || (jwks.find(&kid).is_none()
                        && *expires <= Instant::now() + Duration::from_secs(270))
            }) {
                let raw: serde_json::Value = self
                    .http
                    .json(self.http.request(::http::Method::GET, &self.source))
                    .await?;
                let jwks: JwkSet = serde_json::from_value(raw.clone())?;
                *keys = Some((jwks, raw, Instant::now() + Duration::from_secs(300)));
            }
            let (jwks, raw, _) = keys.as_ref().ok_or(Error::Authentication)?;
            if let Some(channel) = channel {
                let endorsed = raw
                    .pointer("/keys")
                    .unwrap_or(&serde_json::Value::Null)
                    .as_array()
                    .and_then(|keys| {
                        keys.iter().find(|key| {
                            key.get("kid").and_then(serde_json::Value::as_str) == Some(kid.as_str())
                        })
                    })
                    .and_then(|key| {
                        key.pointer("/endorsements")
                            .unwrap_or(&serde_json::Value::Null)
                            .as_array()
                    })
                    .is_some_and(|endorsements| endorsements.iter().any(|value| value == channel));
                if !endorsed {
                    return Err(Error::Authentication);
                }
            }
            let jwk = jwks.find(&kid).ok_or(Error::Authentication)?;
            let key = DecodingKey::from_jwk(jwk).map_err(|_| Error::Authentication)?;
            decode::<T>(token, &key, &self.validation)
                .map(|data| data.claims)
                .map_err(|_| Error::Authentication)
        }
    }
}
#[cfg(any(feature = "teams", feature = "googlechat"))]
pub use jwt::JwtVerifier;
