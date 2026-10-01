#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
#[test]
fn raw_body_and_secret_tampering_fail() -> Result<(), Error> {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"secret").map_err(|_| Error::Authentication)?;
    mac.update(b"body");
    let signature = mac.finalize().into_bytes();
    verify_hmac_sha256(b"secret", b"body", &signature)?;
    assert!(verify_hmac_sha256(b"secret", b"body ", &signature).is_err());
    assert!(verify_hmac_sha256(b"", b"body", &signature).is_err());
    verify_secret(b"secret", b"secret")?;
    assert!(verify_secret(b"secret", b"secreT").is_err());
    Ok(())
}
#[cfg(any(feature = "teams", feature = "googlechat"))]
#[tokio::test]
async fn jwt_rejects_symmetric_tokens_before_fetching_keys() -> Result<(), Error> {
    use jsonwebtoken::{EncodingKey, Header, encode};
    let http = crate::Http::new(std::time::Duration::from_millis(1), 1024)?;
    let verifier = JwtVerifier::new(
        http,
        "https://invalid.test/keys".into(),
        "issuer",
        "audience",
    )?;
    let token = encode(
        &Header::default(),
        &serde_json::json!({"iss":"issuer","aud":"audience","exp":4_000_000_000_u64}),
        &EncodingKey::from_secret(b"secret"),
    )
    .map_err(|_| Error::Invalid("test JWT"))?;
    assert!(matches!(
        verifier.verify::<serde_json::Value>(&token).await,
        Err(Error::Authentication)
    ));
    Ok(())
}
