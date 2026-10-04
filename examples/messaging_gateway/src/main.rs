//! Runs an authenticated messaging gateway with one configured bot and Rig agent.
mod platforms;
#[path = "../../messaging_slack/src/provider.rs"]
mod provider;
mod runtime;
use rig_core::wasm_compat::WasmBoxedFuture;
use rig_messaging::{ChatConfig, ChatRouter, Gate, Inbound};
use rig_messaging_platforms::{Error, Gateway, Platform};
use std::{collections::HashSet, sync::Arc};

fn allowlist(name: &str) -> Option<HashSet<String>> {
    std::env::var(name).ok().map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect()
    })
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = dotenvy::dotenv()
        && !error.not_found()
    {
        return Err(std::io::Error::other("failed to load .env").into());
    }
    let platform_name = std::env::var("MESSAGING_PLATFORM")?;
    let (agent, go) = provider::agent_from_env().await?;
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let (platform, events) = platforms::configured(&platform_name, shutdown_rx.clone()).await?;
    let gate = Gate {
        allowed_channels: allowlist("MESSAGING_ALLOWED_CHANNELS"),
        allowed_users: allowlist("MESSAGING_ALLOWED_USERS"),
        ..Default::default()
    };
    let cfg = ChatConfig {
        attachment_mime_types: if go {
            Some(HashSet::new())
        } else {
            allowlist("MESSAGING_ATTACHMENT_MIME_TYPES")
        },
        ..Default::default()
    };
    let router = Arc::new(ChatRouter::new(agent, gate, cfg));
    let gateway = Arc::new(
        Gateway::new(router, platform, 1024 * 1024, 4096)?.with_dispatch(Arc::new(
            |router: Arc<ChatRouter>,
             platform: Arc<dyn Platform>,
             inbound: Inbound|
             -> WasmBoxedFuture<'static, Result<(), Error>> {
                Box::pin(async move {
                    let key = inbound.reply_channel.session_key();
                    let bot_id = platform.bot_id().to_owned();
                    provider::SESSION
                        .scope(key, router.handle(platform, inbound, &bot_id))
                        .await?;
                    Ok(())
                })
            },
        )),
    );
    let worker=events.map(|(mut events,worker)| {
        let gateway=gateway.clone();
        let mut stopped=shutdown_rx.clone();
        tokio::spawn(async move {
            loop {tokio::select! {
                _=stopped.changed()=>return,
                event=events.recv()=>match event {
                    Some(event)=>if gateway.dispatch_event(event).await.is_err(){eprintln!("messaging event failed");},
                    None=>return,
                },
            }}
        });
        worker
    });
    let path = std::env::var("MESSAGING_WEBHOOK_PATH").unwrap_or_else(|_| "/webhook".into());
    if !path.starts_with('/') || path.contains(['{', '}']) {
        return Err(std::io::Error::other("webhook path must be a literal absolute route").into());
    }
    let listener = tokio::net::TcpListener::bind(
        std::env::var("MESSAGING_LISTEN").unwrap_or_else(|_| "127.0.0.1:3000".into()),
    )
    .await?;
    eprintln!(
        "{platform_name} webhook listening at {}{path}",
        listener.local_addr()?
    );
    let mut server_shutdown = shutdown_rx;
    let server = axum::serve(listener, gateway.route(&path)).with_graceful_shutdown(async move {
        let _ = server_shutdown.changed().await;
    });
    runtime::supervise(
        std::future::IntoFuture::into_future(server),
        worker,
        shutdown,
        tokio::signal::ctrl_c(),
    )
    .await?;
    Ok(())
}
