//! Completion model selection for OpenAI or an explicit OpenCode Go endpoint.
//!
//! Go requests carry the current conversation's session key through HTTP
//! middleware. The endpoint selects the wire protocol.

use rig_agent::{Agent, AgentBuilder};
use rig_core::{
    DynModel,
    memory::InMemoryConversationMemory,
    operation::Completion,
    providers::{
        anthropic::AnthropicConfig,
        openai::{self, OpenAI, OpenAIConfig},
    },
    wasm_compat::WasmBoxedFuture,
};
use rig_http::http_client::{
    self, DynHttpClient, HeaderMap, HeaderValue, Method, Uri, middleware::HttpMiddleware,
};
use rig_reqwest::{ReqwestClient, reqwest};
use std::time::Duration;

tokio::task_local! {
    pub(crate) static SESSION: String;
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Env(#[from] std::env::VarError),
    #[error(transparent)]
    OpenAI(#[from] rig_core::client::env::EnvError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("{0}")]
    Config(&'static str),
}

struct SessionHeader;
impl HttpMiddleware for SessionHeader {
    fn before_request_headers<'a>(
        &'a self,
        _: &'a Method,
        _: &'a Uri,
        headers: &'a mut HeaderMap,
    ) -> WasmBoxedFuture<'a, http_client::Result<()>> {
        Box::pin(async move {
            let session = SESSION.try_with(Clone::clone).map_err(|_| {
                http_client::Error::instance(std::io::Error::other(
                    "missing Go conversation session",
                ))
            })?;
            headers.insert("x-opencode-session", HeaderValue::from_str(&session)?);
            Ok(())
        })
    }
}

#[derive(Debug, PartialEq)]
enum Api {
    Chat,
    Responses,
    Messages,
}
fn endpoint(endpoint: &str) -> Result<(Api, String), Error> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|_| Error::Config("invalid OPENCODE_GO_ENDPOINT URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Config(
            "Go endpoint must be an HTTP(S) URL without query or fragment",
        ));
    }
    let endpoint = url.as_str().trim_end_matches('/');
    for (suffix, api) in [
        ("/v1/chat/completions", Api::Chat),
        ("/v1/responses", Api::Responses),
        ("/v1/messages", Api::Messages),
    ] {
        if let Some(base) = endpoint.strip_suffix(suffix) {
            return Ok((api, base.into()));
        }
    }
    Err(Error::Config(
        "Go endpoint must end in /v1/chat/completions, /v1/responses or /v1/messages",
    ))
}
fn go_model(
    api_key: String,
    model: String,
    endpoint_url: &str,
) -> Result<DynModel<Completion>, Error> {
    if api_key.trim().is_empty() || model.trim().is_empty() || model.starts_with("opencode-go/") {
        return Err(Error::Config(
            "Go requires an API key and a bare model id without the opencode-go/ prefix",
        ));
    }
    let (api, base) = endpoint(endpoint_url)?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("rig-messaging-slack/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()?;
    let http = DynHttpClient::new(ReqwestClient::from(client)).with_middleware(SessionHeader);
    Ok(match api {
        Api::Messages => AnthropicConfig::new(api_key)
            .with_base_url(base)
            .connect(http)
            .completion(model)
            .into(),
        Api::Chat => OpenAIConfig::new(api_key)
            .with_base_url(format!("{base}/v1"))
            .connect(http)
            .chat(model)
            .into(),
        Api::Responses => OpenAIConfig::new(api_key)
            .with_base_url(format!("{base}/v1"))
            .connect(http)
            .responses(model)
            .into(),
    })
}

pub(crate) fn agent_from_env() -> Result<(Agent, bool), Error> {
    let (model, go): (DynModel<Completion>, bool) = match std::env::var("OPENCODE_GO_API_KEY") {
        Ok(key) => (
            go_model(
                key,
                std::env::var("OPENCODE_GO_MODEL")?,
                &std::env::var("OPENCODE_GO_ENDPOINT")?,
            )?,
            true,
        ),
        Err(std::env::VarError::NotPresent) => {
            let provider = OpenAI::from_env()?;
            let model =
                std::env::var("OPENAI_MODEL").unwrap_or_else(|_| openai::GPT_4O_MINI.into());
            (provider.completion(model).into(), false)
        }
        Err(error) => return Err(error.into()),
    };
    let mut agent = AgentBuilder::new(model)
        .memory(InMemoryConversationMemory::new())
        .default_max_turns(3);
    if go {
        let tokens = match std::env::var("OPENCODE_GO_MAX_TOKENS") {
            Ok(value) => value
                .parse::<std::num::NonZeroU64>()
                .map_err(|_| Error::Config("OPENCODE_GO_MAX_TOKENS must be a positive integer"))?
                .get(),
            Err(std::env::VarError::NotPresent) => 4096,
            Err(error) => return Err(error.into()),
        };
        agent = agent.max_tokens(tokens);
    }
    Ok((agent.build(), go))
}

#[cfg(test)]
mod tests;
