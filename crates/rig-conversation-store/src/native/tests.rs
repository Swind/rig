use super::*;
use bytes::Bytes;
use rig_core::http_client::{
    self, DynHttpClient, HttpClientExt, LazyBody, MultipartForm, Request, Response,
    StreamingResponse,
};
use rig_core::{
    conversation_search::{ConversationSearch, ConversationSearchRequest},
    providers::openai,
    tool::{IntoToolOutput, PortableTool, builtin::SearchConversationsTool},
};
use serde_json::json;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use testcontainers::{
    GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::sync::Notify;

#[derive(Clone)]
struct GatedHttp {
    http: DynHttpClient,
    enabled: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl GatedHttp {
    fn new() -> Self {
        Self {
            http: DynHttpClient::new(rig_reqwest::shared()),
            enabled: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }
}

impl HttpClientExt for GatedHttp {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let response = self.http.send(request);
        let enabled = self.enabled.load(Ordering::SeqCst);
        let entered = self.entered.clone();
        let release = self.release.clone();
        async move {
            if enabled {
                entered.notify_one();
                release.notified().await;
            }
            response.await
        }
    }

    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        self.http.send_multipart(request)
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        self.http.send_streaming(request)
    }
}

fn embedding_model(server: &httpmock::MockServer, http: GatedHttp) -> DynModel<Embedding> {
    for (text, vector) in [
        ("target", vec![1.0, 0.0, 0.0]),
        ("context", vec![0.0, 1.0, 0.0]),
        ("distractor", vec![0.8, 0.2, 0.0]),
    ] {
        server.mock(|when, then| {
            when.method(httpmock::Method::POST)
                .path("/embeddings")
                .body_includes(text);
            then.status(200).json_body(json!({
                "object":"list", "data":[{"object":"embedding","embedding":vector,"index":0}],
                "model":"text-embedding-ada-002","usage":{"prompt_tokens":1,"total_tokens":1}
            }));
        });
    }
    openai::OpenAIConfig::new("test")
        .with_base_url(server.base_url())
        .connect(http)
        .embedding(openai::TEXT_EMBEDDING_ADA_002, None)
        .erase()
}

fn search_request(limit: u32) -> ConversationSearchRequest {
    ConversationSearchRequest {
        query: "target".into(),
        limit,
        conversation_id: None,
    }
}

#[test]
fn rejects_unbounded_configuration() -> anyhow::Result<()> {
    let mut config = StoreConfig::new("scope", "model-v1", 3);
    config.validate()?;
    config.max_output_bytes = 1;
    anyhow::ensure!(config.validate().is_err());
    config.max_output_bytes = 100;
    config.max_candidates = 0;
    anyhow::ensure!(config.validate().is_err());
    config.max_candidates = 10;
    config.scope = " \n".into();
    anyhow::ensure!(config.validate().is_err());
    Ok(())
}

#[tokio::test]
async fn qdrant_originals_retry_reopen_clear_and_tool() -> anyhow::Result<()> {
    let container = GenericImage::new("qdrant/qdrant", "v1.17.0")
        .with_exposed_port(6334.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Qdrant gRPC listening on 6334"))
        .start()
        .await?;
    let client = qdrant_client::Qdrant::from_url(&format!(
        "http://{}:{}",
        container.get_host().await?,
        container.get_host_port_ipv4(6334).await?
    ))
    .build()?;
    let directory = tempfile::tempdir()?;
    let sqlite = directory.path().join("archive.sqlite");
    let server = httpmock::MockServer::start();
    let gated = GatedHttp::new();
    let model = embedding_model(&server, gated.clone());
    let config = StoreConfig::new("scope-a", "model-v1", 3);
    let store = ConversationStore::open(
        &sqlite,
        client.clone(),
        "conversations",
        model.clone(),
        config.clone(),
    )
    .await?;
    let first = ConversationId::new("first");
    let second = ConversationId::new("second");
    store.append(&first, vec![Message::user("target")]).await?;
    store
        .append(&first, vec![Message::assistant("context")])
        .await?;
    store
        .append(&second, vec![Message::user("distractor")])
        .await?;
    anyhow::ensure!(
        store.load(&first).await? == vec![Message::user("target"), Message::assistant("context")]
    );
    anyhow::ensure!(store.index_status().await?.pending_jobs == 3);
    anyhow::ensure!(store.search(search_request(2)).await?.is_empty());
    store.process_pending(1).await?;
    anyhow::ensure!(store.index_status().await?.pending_jobs == 2);
    let lagging_hits = store.search(search_request(2)).await?;
    anyhow::ensure!(lagging_hits.len() == 2);
    anyhow::ensure!(
        lagging_hits
            .last()
            .is_some_and(|hit| hit.conversation_id == first
                && hit.start_index == 1
                && hit.messages == vec![Message::assistant("context")]),
        "SQLite context must be available before its Qdrant projection"
    );
    store.process_pending(10).await?;
    anyhow::ensure!(store.index_status().await?.pending_jobs == 0);
    let hits = store.search(search_request(2)).await?;
    anyhow::ensure!(hits.len() == 2);
    anyhow::ensure!(hits.iter().all(|hit| hit.conversation_id == first));
    anyhow::ensure!(hits.iter().map(|hit| hit.start_index).collect::<Vec<_>>() == vec![0, 1]);
    anyhow::ensure!(
        hits.first()
            .is_some_and(|hit| hit.messages == vec![Message::user("target")])
    );
    let tool = SearchConversationsTool::new(store.clone());
    let output = tool.call(search_request(2)).await?.into_tool_output()?;
    anyhow::ensure!(output.as_json() == Some(&serde_json::to_value(&hits)?));
    let mut scoped = search_request(2);
    scoped.conversation_id = Some(second.clone());
    anyhow::ensure!(
        store
            .search(scoped)
            .await?
            .iter()
            .all(|hit| hit.conversation_id == second)
    );

    // An external outage leaves original history and stable retry work intact.
    client.delete_collection("conversations").await?;
    store.append(&first, vec![Message::user("target")]).await?;
    anyhow::ensure!(store.process_pending(10).await.is_err());
    anyhow::ensure!(store.index_status().await?.failed_attempts > 0);
    anyhow::ensure!(store.load(&first).await?.len() == 3);
    anyhow::ensure!(store.search(search_request(2)).await.is_err());
    qdrant::VectorProjection::open(client.clone(), "conversations".into(), 3).await?;
    store.rebuild_indexes().await?;
    store.process_pending(10).await?;
    anyhow::ensure!(store.index_status().await?.pending_jobs == 0);
    store.clear(&first).await?;
    anyhow::ensure!(store.load(&first).await?.is_empty());
    anyhow::ensure!(
        store
            .search(search_request(2))
            .await?
            .iter()
            .all(|hit| hit.conversation_id != first)
    );
    store.append(&first, vec![Message::user("target")]).await?;
    store.process_pending(10).await?;
    anyhow::ensure!(
        store
            .search(search_request(1))
            .await?
            .first()
            .is_some_and(|hit| hit.start_index == 0 && hit.conversation_id == first)
    );
    drop(tool);
    drop(store);
    let reopened = ConversationStore::open(&sqlite, client, "conversations", model, config).await?;
    anyhow::ensure!(reopened.load(&first).await? == vec![Message::user("target")]);
    anyhow::ensure!(reopened.index_status().await?.pending_jobs == 0);

    server.mock(|when, then| {
        when.method(httpmock::Method::POST).path("/chat/completions").body_includes("capture-success");
        then.status(200).json_body(json!({"id":"captured","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"context"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}));
    });
    server.mock(|when, then| {
        when.method(httpmock::Method::POST)
            .path("/chat/completions")
            .body_includes("failed-turn");
        then.status(503)
            .json_body(json!({"error":{"message":"offline","type":"server_error"}}));
    });
    let completion = openai::OpenAIConfig::new("test")
        .with_base_url(server.base_url())
        .client()
        .chat("mock");
    let agent = rig_agent::agent::AgentBuilder::new(completion)
        .memory(reopened.clone())
        .build();
    let captured_id = ConversationId::new("agent-memory");
    let response = agent
        .prompt("target capture-success")
        .conversation(captured_id.clone())
        .await?;
    anyhow::ensure!(response.output == "context");
    let captured = reopened.load(&captured_id).await?;
    anyhow::ensure!(
        captured.len() == 2 && captured.first() == Some(&Message::user("target capture-success"))
    );
    anyhow::ensure!(
        matches!(captured.last(), Some(Message::Assistant { content, .. })
        if matches!(content.as_slice(), [rig_core::completion::AssistantContent::Text(text)] if text.text == "context"))
    );
    let failed_id = ConversationId::new("failed-memory");
    anyhow::ensure!(
        agent
            .prompt("failed-turn")
            .conversation(failed_id.clone())
            .await
            .is_err()
    );
    anyhow::ensure!(reopened.load(&failed_id).await?.is_empty());

    let cancel_id = ConversationId::new("cancelled-indexing");
    reopened
        .append(&cancel_id, vec![Message::user("target")])
        .await?;
    gated.enabled.store(true, Ordering::SeqCst);
    let processing_store = reopened.clone();
    let processing = tokio::spawn(async move { processing_store.process_pending(10).await });
    tokio::time::timeout(Duration::from_secs(5), gated.entered.notified()).await?;
    processing.abort();
    anyhow::ensure!(processing.await.is_err());
    let clearing_store = reopened.clone();
    let cleared_id = cancel_id.clone();
    let clearing = tokio::spawn(async move { clearing_store.clear(&cleared_id).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    anyhow::ensure!(
        !clearing.is_finished(),
        "clear must wait for owned projection work after caller cancellation"
    );
    gated.enabled.store(false, Ordering::SeqCst);
    gated.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), clearing).await???;
    anyhow::ensure!(reopened.load(&cancel_id).await?.is_empty());
    reopened.process_pending(10).await?;
    anyhow::ensure!(reopened.index_status().await?.pending_jobs == 0);
    Ok(())
}
