#![allow(clippy::panic_in_result_fn)]

use super::*;
use crate::test_utils::{MockHttpResponse, SequencedHttpClient, SequencedStreamingHttpClient};
use bytes::Bytes;
use serde_json::json;

fn data(context: u32) -> String {
    json!({"opencode-go":{"models":{"fixture":{
        "name":"Go fixture", "limit":{"context":context,"output":2048},
        "tool_call":true, "modalities":{"input":["text","image"],"output":["text"]},
        "cost":{"input":1,"output":2}
    }}},"unknown-vendor":{"models":{"fixture":{}}}})
    .to_string()
}

#[tokio::test]
async fn clones_share_fetches_and_invalidation_refreshes_the_catalog() -> anyhow::Result<()> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::success(data(100_000)),
        MockHttpResponse::success(data(200_000)),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    let clone = cache.clone();
    let (first, second) = futures::join!(cache.load(), clone.load());
    for (catalog, skipped) in [first?, second?] {
        let spec = catalog.resolve("opencode-go/fixture")?.spec;
        assert_eq!(spec.context_window, Some(100_000));
        assert_eq!(spec.max_output_tokens, Some(2048));
        assert!(spec.tools);
        assert!(spec.input.image);
        assert!(spec.pricing.is_some());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].reason, super::super::SkipReason::UnknownVendor);
    }
    assert_eq!(http.requests().len(), 1);
    assert!(!http.requests()[0].headers.contains_key("authorization"));
    cache.invalidate().await;
    let catalog = clone.catalog().await?;
    assert_eq!(
        catalog.resolve("opencode-go/fixture")?.spec.context_window,
        Some(200_000)
    );
    assert_eq!(http.requests().len(), 2);
    Ok(())
}

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
    let http = SequencedHttpClient::new([MockHttpResponse::success(data(100_000))]);
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
        async move { cache.catalog().await }
    });
    started.notified().await;
    let second = tokio::spawn({
        let cache = cache.clone();
        async move { cache.catalog().await }
    });
    release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        first.await??;
        second.await??;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert_eq!(http.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn failed_fetches_do_not_poison_the_cache() -> anyhow::Result<()> {
    let http = SequencedHttpClient::new([
        MockHttpResponse::ErrorResponse(StatusCode::SERVICE_UNAVAILABLE, Bytes::new()),
        MockHttpResponse::success("not-json"),
        MockHttpResponse::success("[]"),
        MockHttpResponse::success(data(100_000)),
    ]);
    let cache = ModelsDev::new(DynHttpClient::new(http.clone()));
    assert!(matches!(
        cache.catalog().await,
        Err(Error::Status(StatusCode::SERVICE_UNAVAILABLE))
    ));
    assert!(matches!(cache.catalog().await, Err(Error::Catalog(_))));
    assert!(matches!(cache.catalog().await, Err(Error::Catalog(_))));
    assert!(
        cache
            .catalog()
            .await?
            .resolve("opencode-go/fixture")
            .is_ok()
    );
    assert_eq!(http.requests().len(), 4);
    Ok(())
}

#[tokio::test]
async fn size_limits_apply_across_chunks_and_stream_errors_propagate() {
    let http = SequencedStreamingHttpClient::new(vec![
        Ok(Bytes::from(vec![b' '; BODY_LIMIT])),
        Ok(Bytes::from_static(b" ")),
    ]);
    assert!(matches!(
        ModelsDev::new(DynHttpClient::new(http)).catalog().await,
        Err(Error::BodyLimit)
    ));
    let http = SequencedStreamingHttpClient::new(vec![Err(http_client::Error::StreamEnded)]);
    assert!(matches!(
        ModelsDev::new(DynHttpClient::new(http)).catalog().await,
        Err(Error::Http(http_client::Error::StreamEnded))
    ));
}
