//! Completion model selection for OpenAI or an explicit OpenCode Go endpoint.
//!
//! Go requests carry the current conversation's session key through HTTP
//! middleware. The endpoint selects the wire protocol.

use rig_agent::{
    Agent, AgentBuilder,
    agent::{AgentHook, DispatchAction, DispatchEvent, HookContext},
};
use rig_core::{
    DynModel,
    effect::EffectKind,
    memory::InMemoryConversationMemory,
    model::models_dev::ModelsDev,
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
use std::{sync::OnceLock, time::Duration};

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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ModelLimits {
    context: Option<u64>,
    output: Option<u64>,
}

impl ModelLimits {
    fn budget(self, requested: u64, input_bytes: usize) -> Option<u64> {
        let output = requested.min(self.output.unwrap_or(requested));
        let remaining = self.context.map(|context| {
            // ponytail: serialized UTF-8 bytes plus a margin estimate input cost;
            // use a model tokenizer when precise context utilization is needed.
            context.saturating_sub((input_bytes as u64).saturating_add(4096))
        });
        let output = output.min(remaining.unwrap_or(output));
        (output > 0).then_some(output)
    }
}

impl AgentHook for ModelLimits {
    async fn on_dispatch(&self, _: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let EffectKind::Completion { request, stream } = event.kind else {
            return DispatchAction::proceed();
        };
        let input = match serde_json::to_vec(request) {
            Ok(input) => input,
            Err(_) => return DispatchAction::stop("could not estimate Go request size"),
        };
        let Some(tokens) = self.budget(request.max_tokens.unwrap_or(4096), input.len()) else {
            return DispatchAction::stop("Go conversation exceeds the estimated context budget");
        };
        let mut request = request.clone();
        request.max_tokens = Some(tokens);
        DispatchAction::patch(EffectKind::Completion {
            request,
            stream: *stream,
        })
    }
}

fn model_catalog() -> Result<&'static ModelsDev, Error> {
    static CATALOG: OnceLock<ModelsDev> = OnceLock::new();
    if let Some(catalog) = CATALOG.get() {
        return Ok(catalog);
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    Ok(CATALOG.get_or_init(|| ModelsDev::new(DynHttpClient::new(ReqwestClient::from(client)))))
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

pub(crate) async fn agent_from_env() -> Result<(Agent, bool), Error> {
    let mut limits = ModelLimits::default();
    let (model, go): (DynModel<Completion>, bool) = match std::env::var("OPENCODE_GO_API_KEY") {
        Ok(key) => {
            let id = std::env::var("OPENCODE_GO_MODEL")?;
            let model = go_model(key, id.clone(), &std::env::var("OPENCODE_GO_ENDPOINT")?)?;
            limits = match model_catalog()?.get("opencode-go", &id).await {
                Ok(info) => ModelLimits {
                    context: info
                        .as_ref()
                        .and_then(|info| info.context_length)
                        .map(u64::from),
                    output: info.and_then(|info| info.max_output_tokens).map(u64::from),
                },
                Err(_) => {
                    eprintln!("Go model metadata unavailable; using configured output limit");
                    ModelLimits::default()
                }
            };
            eprintln!(
                "Go model {id}: context={:?}, output={:?}",
                limits.context, limits.output
            );
            (model, true)
        }
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
        agent = agent.max_tokens(tokens).add_hook(limits);
    }
    Ok((agent.build(), go))
}

#[cfg(test)]
mod tests;
