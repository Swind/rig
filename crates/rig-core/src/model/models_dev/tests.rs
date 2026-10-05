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
async fn overlapping_queries_share_an_in_flight_fetch() -> anyhow::Result<()> {
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
        Ok::<_, anyhow::Error>((first.await??, second.await??))
    })
    .await??;
    anyhow::ensure!(first.is_some(), "first query should find the model");
    anyhow::ensure!(second.is_some(), "second query should find the model");
    anyhow::ensure!(http.requests().len() == 1, "queries should share one fetch");
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
fn limits_are_provider_scoped_and_missing_metadata_stays_absent() -> anyhow::Result<()> {
    let catalog = serde_json::from_str(&catalog())?;
    let first = lookup(&catalog, "first", "shared").ok_or(Error::InvalidCatalog)?;
    anyhow::ensure!(
        first.context_length == Some(1000000),
        "context limit changed"
    );
    anyhow::ensure!(
        first.max_output_tokens == Some(384000),
        "output limit changed"
    );
    anyhow::ensure!(first.name.as_deref() == Some("First"), "model name changed");
    anyhow::ensure!(
        lookup(&catalog, "second", "shared").and_then(|info| info.context_length) == Some(8192),
        "context limits must remain provider-scoped"
    );
    for id in ["invalid", "oversized"] {
        let info = lookup(&catalog, "first", id).ok_or(Error::InvalidCatalog)?;
        anyhow::ensure!(
            info.context_length.is_none(),
            "{id} context limit must be absent"
        );
        anyhow::ensure!(
            info.max_output_tokens.is_none(),
            "{id} output limit must be absent"
        );
    }
    anyhow::ensure!(
        lookup(&catalog, "unknown", "shared").is_none(),
        "unknown provider must not resolve"
    );
    anyhow::ensure!(
        lookup(&catalog, "first", "unknown").is_none(),
        "unknown model must not resolve"
    );
    Ok(())
}

#[tokio::test]
async fn clones_share_fetches_and_invalidation_refreshes_the_catalog() -> anyhow::Result<()> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::success(catalog()),
        MockHttpResponse::success(catalog()),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    let clone = cache.clone();
    let (first, second) =
        futures::join!(cache.get("first", "shared"), clone.get("second", "shared"));
    anyhow::ensure!(first?.is_some(), "first provider model must resolve");
    anyhow::ensure!(second?.is_some(), "second provider model must resolve");
    anyhow::ensure!(
        clone.get("missing", "missing").await?.is_none(),
        "missing model must remain absent"
    );
    anyhow::ensure!(http.requests().len() == 1, "clones should share one fetch");
    anyhow::ensure!(
        !http.requests()[0].headers.contains_key("authorization"),
        "catalog requests must not include authorization"
    );
    cache.invalidate().await;
    anyhow::ensure!(
        clone.get("first", "shared").await?.is_some(),
        "model must resolve after invalidation"
    );
    anyhow::ensure!(
        http.requests().len() == 2,
        "invalidation should trigger a fetch"
    );
    Ok(())
}

#[tokio::test]
async fn failed_fetches_do_not_poison_the_cache() -> anyhow::Result<()> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::ErrorResponse(StatusCode::SERVICE_UNAVAILABLE, Bytes::new()),
        MockHttpResponse::success("not-json"),
        MockHttpResponse::success("[]"),
        MockHttpResponse::success(catalog()),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    anyhow::ensure!(
        matches!(
            cache.get("first", "shared").await,
            Err(Error::Status(StatusCode::SERVICE_UNAVAILABLE))
        ),
        "unavailable response must return a status error"
    );
    anyhow::ensure!(
        matches!(cache.get("first", "shared").await, Err(Error::Json(_))),
        "malformed JSON must return a JSON error"
    );
    anyhow::ensure!(
        matches!(
            cache.get("first", "shared").await,
            Err(Error::InvalidCatalog)
        ),
        "invalid catalog structure must return an invalid catalog error"
    );
    anyhow::ensure!(
        cache.get("first", "shared").await?.is_some(),
        "successful fetch must resolve the model after failures"
    );
    anyhow::ensure!(
        http.requests().len() == 4,
        "failed fetches must remain retryable"
    );
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
