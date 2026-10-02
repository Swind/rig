#![allow(clippy::panic_in_result_fn, clippy::indexing_slicing)]
use super::*;
use rig_agent::AgentBuilder;
use rig_core::{memory::InMemoryConversationMemory, test_utils::MockCompletionModel};
use rig_messaging::{ChannelRef, ChatAdapter, ChatConfig, ChatError, Gate, MessageRef, Sender};
use std::sync::atomic::{AtomicUsize, Ordering};

struct FakePlatform(AtomicUsize, AtomicUsize);
impl ChatAdapter for FakePlatform {
    fn platform(&self) -> &'static str {
        "fake"
    }
    fn message_limit(&self) -> usize {
        100
    }
    fn send<'a>(
        &'a self,
        _: &'a ChannelRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<MessageRef, ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("send")) })
    }
    fn edit<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("edit")) })
    }
    fn delete<'a>(&'a self, _: &'a MessageRef) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("delete")) })
    }
    fn add_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("reaction")) })
    }
    fn remove_reaction<'a>(
        &'a self,
        _: &'a MessageRef,
        _: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), ChatError>> {
        Box::pin(async { Err(ChatError::Unsupported("reaction")) })
    }
}
impl Platform for FakePlatform {
    fn bot_id(&self) -> &str {
        "bot"
    }
    fn receive(&self, _: WebhookRequest) -> WasmBoxedFuture<'_, Result<WebhookResponse, Error>> {
        Box::pin(async { Err(Error::Authentication) })
    }
    fn prepare(&self, event: Incoming) -> WasmBoxedFuture<'_, Result<Inbound, Error>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::Relaxed);
            if self
                .1
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                    count.checked_sub(1)
                })
                .is_ok()
            {
                Err(Error::Invalid("simulated prepare failure"))
            } else {
                Ok(event.inbound)
            }
        })
    }
}
fn event(mention: bool) -> Incoming {
    let channel = ChannelRef {
        platform: "fake".into(),
        scope_id: None,
        channel_id: "group".into(),
        thread_id: None,
    };
    Incoming {
        inbound: Inbound {
            message: MessageRef {
                channel: channel.clone(),
                message_id: "1".into(),
            },
            reply_channel: channel,
            sender: Sender {
                id: "human".into(),
                name: "Human".into(),
                is_bot: false,
            },
            text: "hello".into(),
            attachments: Vec::new(),
            is_dm: false,
            is_thread: false,
            mentions_bot: mention,
            context: rig_messaging::MessageContext::default(),
        },
        payload: serde_json::Value::Null,
    }
}
#[tokio::test]
async fn gate_precedes_media_and_duplicates_do_not_dispatch() -> Result<(), Error> {
    let model = MockCompletionModel::from_stream_turns(std::iter::empty::<
        Vec<rig_core::test_utils::MockStreamEvent>,
    >());
    let agent = AgentBuilder::new(model)
        .memory(InMemoryConversationMemory::new())
        .build();
    let router = Arc::new(ChatRouter::new(
        agent,
        Gate::default(),
        ChatConfig::default(),
    ));
    let platform = Arc::new(FakePlatform(AtomicUsize::new(0), AtomicUsize::new(0)));
    let runs = Arc::new(AtomicUsize::new(0));
    let callback_runs = runs.clone();
    let gateway = Arc::new(
        Gateway::new(router, platform.clone(), 1024, 10)?.with_dispatch(Arc::new(
            move |_: Arc<ChatRouter>,
                  _: Arc<dyn Platform>,
                  _: Inbound|
                  -> WasmBoxedFuture<'static, Result<(), Error>> {
                let runs = callback_runs.clone();
                Box::pin(async move {
                    runs.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                })
            },
        )),
    );
    gateway.dispatch_event(event(false)).await?;
    assert_eq!(platform.0.load(Ordering::Relaxed), 0);
    gateway.dispatch_event(event(true)).await?;
    gateway.dispatch_event(event(true)).await?;
    tokio::task::yield_now().await;
    assert_eq!(platform.0.load(Ordering::Relaxed), 1);
    assert_eq!(runs.load(Ordering::Relaxed), 1);
    platform.1.store(1, Ordering::Relaxed);
    let mut retry = event(true);
    retry.inbound.message.message_id = "retry".into();
    assert!(gateway.dispatch_event(retry.clone()).await.is_err());
    gateway.dispatch_event(retry.clone()).await?;
    gateway.dispatch_event(retry).await?;
    tokio::task::yield_now().await;
    assert_eq!(platform.0.load(Ordering::Relaxed), 3);
    assert_eq!(runs.load(Ordering::Relaxed), 2);
    let request = WebhookRequest {
        method: ::http::Method::POST,
        headers: ::http::HeaderMap::new(),
        query: Default::default(),
        body: bytes::Bytes::new(),
    };
    assert!(matches!(
        gateway.receive(request).await,
        Err(Error::Authentication)
    ));
    assert_eq!(runs.load(Ordering::Relaxed), 2);
    Ok(())
}
