use super::*;
use crate::test_utils::{MockHttpResponse, SequencedHttpClient, SequencedStreamingHttpClient};
use bytes::Bytes;
use serde_json::json;

struct PauseFetch {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl crate::http_client::middleware::HttpMiddleware for PauseFetch {
    fn before_request_headers<'a>(
        &'a self,
        _: &'a http_client::Method,
        _: &'a Uri,
        _: &'a mut http_client::HeaderMap,
    ) -> crate::wasm_compat::WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            Ok(())
        })
    }
}

#[tokio::test]
async fn overlapping_queries_share_an_in_flight_fetch() -> Result<(), Box<dyn std::error::Error>> {
    let http = SequencedHttpClient::new([MockHttpResponse::success(catalog())]);
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let cache = ModelsDev::new(
        DynHttpClient::new(http.clone()).with_middleware(PauseFetch {
            started: started.clone(),
            release: release.clone(),
        }),
    );
    let first = tokio::spawn({
        let cache = cache.clone();
        async move { cache.get("first", "shared").await }
    });
    started.notified().await;
    let attempted = Arc::new(tokio::sync::Notify::new());
    let second = tokio::spawn({
        let cache = cache.clone();
        let attempted = attempted.clone();
        async move {
            attempted.notify_one();
            cache.get("second", "shared").await
        }
    });
    attempted.notified().await;
    release.notify_one();
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        Ok::<_, Box<dyn std::error::Error>>((first.await??, second.await??))
    })
    .await??;
    assert!(first.is_some());
    assert!(second.is_some());
    assert_eq!(http.requests().len(), 1);
    Ok(())
}

fn catalog() -> String {
    json!({"first":{"models":{
        "shared":{"name":"First","limit":{"context":1000000,"output":384000}},
        "invalid":{"limit":{"context":-1,"output":0}},
        "oversized":{"limit":{"context":4294967296_u64,"output":"4096"}}
    }},"second":{"models":{"shared":{"limit":{"context":8192,"output":1024}}}}})
    .to_string()
}

#[test]
fn limits_are_provider_scoped_and_missing_metadata_stays_absent() -> Result<(), Error> {
    let catalog = serde_json::from_str(&catalog())?;
    let first = lookup(&catalog, "first", "shared").ok_or(Error::InvalidCatalog)?;
    assert_eq!(first.context_length, Some(1000000));
    assert_eq!(first.max_output_tokens, Some(384000));
    assert_eq!(first.name.as_deref(), Some("First"));
    assert_eq!(
        lookup(&catalog, "second", "shared").and_then(|info| info.context_length),
        Some(8192)
    );
    for id in ["invalid", "oversized"] {
        let info = lookup(&catalog, "first", id).ok_or(Error::InvalidCatalog)?;
        assert_eq!(info.context_length, None);
        assert_eq!(info.max_output_tokens, None);
    }
    assert!(lookup(&catalog, "unknown", "shared").is_none());
    assert!(lookup(&catalog, "first", "unknown").is_none());
    Ok(())
}

#[tokio::test]
async fn clones_share_fetches_and_invalidation_refreshes_the_catalog() -> Result<(), Error> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::success(catalog()),
        MockHttpResponse::success(catalog()),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    let clone = cache.clone();
    let (first, second) =
        futures::join!(cache.get("first", "shared"), clone.get("second", "shared"));
    assert!(first?.is_some());
    assert!(second?.is_some());
    assert!(clone.get("missing", "missing").await?.is_none());
    assert_eq!(http.requests().len(), 1);
    assert!(!http.requests()[0].headers.contains_key("authorization"));
    cache.invalidate().await;
    assert!(clone.get("first", "shared").await?.is_some());
    assert_eq!(http.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn failed_fetches_do_not_poison_the_cache() -> Result<(), Error> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::ErrorResponse(StatusCode::SERVICE_UNAVAILABLE, Bytes::new()),
        MockHttpResponse::success("not-json"),
        MockHttpResponse::success("[]"),
        MockHttpResponse::success(catalog()),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    assert!(matches!(
        cache.get("first", "shared").await,
        Err(Error::Status(StatusCode::SERVICE_UNAVAILABLE))
    ));
    assert!(matches!(
        cache.get("first", "shared").await,
        Err(Error::Json(_))
    ));
    assert!(matches!(
        cache.get("first", "shared").await,
        Err(Error::InvalidCatalog)
    ));
    assert!(cache.get("first", "shared").await?.is_some());
    assert_eq!(http.requests().len(), 4);
    Ok(())
}

#[tokio::test]
async fn size_limits_apply_across_chunks_and_stream_errors_propagate() {
    let http = SequencedStreamingHttpClient::new(vec![
        Ok(Bytes::from(vec![b' '; BODY_LIMIT])),
        Ok(Bytes::from_static(b" ")),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http));
    assert!(matches!(
        cache.get("first", "shared").await,
        Err(Error::BodyLimit)
    ));
    let http = SequencedStreamingHttpClient::new(vec![Err(http_client::Error::StreamEnded)]);
    let cache = ModelsDev::new(DynHttpClient::new(http));
    assert!(matches!(
        cache.get("first", "shared").await,
        Err(Error::Http(http_client::Error::StreamEnded))
    ));
}
