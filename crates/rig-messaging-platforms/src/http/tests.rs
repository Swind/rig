#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
#[tokio::test]
async fn downloads_reject_untrusted_urls_before_requests() -> Result<(), Error> {
    let http = Http::new(Duration::from_secs(1), 1024)?;
    for url in [
        "http://trusted.test/file",
        "https://evil.test/file",
        "https://user:secret@trusted.test/file",
        "https://trusted.test:444/file",
    ] {
        assert!(matches!(
            http.download(url, &["trusted.test"], Some("secret"), 10)
                .await,
            Err(Error::Invalid(_))
        ));
    }
    Ok(())
}
#[tokio::test]
async fn http_errors_omit_tokens_in_urls() -> Result<(), Error> {
    let http = Http::new(Duration::from_millis(100), 1024)?;
    let error = http
        .execute(http.request(Method::GET, "http://127.0.0.1:1/bot-secret?token=secret"))
        .await
        .err()
        .ok_or(Error::Invalid("expected transport error"))?;
    assert!(!error.to_string().contains("secret"));
    Ok(())
}
#[tokio::test]
async fn declared_and_chunked_responses_respect_operation_limits()
-> Result<(), Box<dyn std::error::Error>> {
    use axum::{Router, body::Body, routing::get};
    let app = Router::new()
        .route("/declared", get(|| async { vec![0_u8; 100] }))
        .route(
            "/chunked",
            get(|| async {
                Body::from_stream(futures::stream::iter([
                    Ok::<_, std::io::Error>(Bytes::from_static(b"1234")),
                    Ok(Bytes::from_static(b"5678")),
                    Ok(Bytes::from_static(b"9012")),
                ]))
            }),
        )
        .route(
            "/error",
            get(|| async {
                (
                    ::http::StatusCode::BAD_REQUEST,
                    [("content-type", "application/json")],
                    "{\"code\":\"bad\"}",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let http = Http::new(Duration::from_secs(1), 1024)?;
    for path in ["declared", "chunked"] {
        assert!(matches!(
            http.execute_limited(
                http.request(Method::GET, &format!("http://{address}/{path}")),
                8
            )
            .await,
            Err(Error::TooLarge)
        ));
    }
    let (status, headers, body) = http
        .response(http.request(Method::GET, &format!("http://{address}/error")))
        .await?;
    assert_eq!(status, ::http::StatusCode::BAD_REQUEST);
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(body, Bytes::from_static(b"{\"code\":\"bad\"}"));
    server.abort();
    Ok(())
}
