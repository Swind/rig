//! Copilot's model-listing wires: `GET /models` with the editor envelope,
//! read as the shared [`ModelList`] or as typed [`CopilotModel`] entries
//! that name each model's endpoints and reasoning controls.
//!
//! ```no_run
//! use rig_core::providers::copilot::Copilot;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let copilot = Copilot::from_env()?;
//! let models = copilot.models().await?;
//! if let Some(model) = models.iter().find(|model| model.id == "claude-opus-4.7") {
//!     let completion = copilot.completion_for(model);
//!     # let _ = completion;
//! }
//! # Ok(())
//! # }
//! ```

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::CopilotIntent;
use super::PROVIDER_NAME;
use super::wire::{CopilotConfig, REQUEST_ID_HEADER, stamp};
use crate::catalog::{Catalog, ModelSpec, ReasoningSupport};
use crate::completion::Effort;
use crate::error::{EncodeError, ProviderError};
use crate::json_utils::Lenient;
use crate::model::{ModelInfo, ModelList};
use crate::operation::{ModelListing, ModelPage, Whole};
use crate::providers::internal::wire::classify_untyped_line;
use crate::providers::registry::ProviderId;
use crate::wire::{
    Body, Call, Decoder, Descriptor, Encoded, Flow, Framing, Free, Mode, Operation, Out, Wire,
    WireEvent, WireFrame,
};

use super::wire::CopilotEndpoint;

/// Catalogue endpoint returning all models without pagination.
pub(crate) const MODEL_LISTING_PATH: &str = "/models";

impl CopilotConfig {
    /// The model-listing wire.
    pub(crate) fn models(&self) -> Models {
        Models {
            provider: self.clone(),
        }
    }

    /// The typed model-listing wire.
    pub(crate) fn model_details(&self) -> ModelDetails {
        ModelDetails {
            provider: self.clone(),
        }
    }

    /// `GET /models` with the editor envelope.
    fn listing_request(&self) -> Result<Encoded, EncodeError> {
        let mut request = http::Request::get(self.uri(MODEL_LISTING_PATH))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(Body::empty())?;
        stamp(
            &mut request,
            self.api_key.expose(),
            "user",
            false,
            CopilotIntent::Panel,
        )?;
        Ok(Encoded::new(request, Framing::Whole).with_request_id_header(REQUEST_ID_HEADER))
    }
}

/// Copilot's model-listing wire.
///
/// `GET /models` answers with the whole catalogue, so a page never names a
/// next one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Models {
    /// Which Copilot, and how to reach it.
    pub provider: CopilotConfig,
}

/// The model-listing decoder: the catalogue's `data`, each entry naming
/// its id, its name, its vendor and its modality under `capabilities.type`.
#[derive(Default)]
pub struct ModelsDecoder;

impl<'id> Decoder<'id, ModelListing> for ModelsDecoder {
    type Event = Value;

    fn classify(&self, frame: WireFrame) -> WireEvent<Self::Event> {
        classify_untyped_line(frame.as_str().as_bytes())
    }

    fn decode(&mut self, event: Value, out: Out<'id, ModelListing>) -> Result<Flow, ProviderError> {
        let text = |entry: &Value, pointer: &str| {
            entry.at(pointer).and_then(Value::as_str).map(str::to_owned)
        };
        let models = event
            .arr("data")
            .iter()
            .filter_map(|entry| {
                let mut model = ModelInfo::from_id(entry.str("id")?);
                model.name = text(entry, "/name");
                model.owned_by = text(entry, "/vendor");
                model.r#type = text(entry, "/capabilities/type");
                Some(model)
            })
            .collect();
        Ok(out.end(ModelPage {
            models: ModelList::new(models),
            next: None,
        }))
    }
}

impl Wire for Models {
    type Op = ModelListing;
    type Payload = crate::wire::Encoded;
    type Frame = crate::wire::WireFrame;
    type Decoder<'id> = ModelsDecoder;
    type Reassembler = crate::wire::document::Unreassembled;

    fn describe(&self) -> Descriptor<'_> {
        Descriptor::new(PROVIDER_NAME)
    }

    fn encode(&self, _cursor: Option<String>, _mode: Mode) -> Result<Encoded, EncodeError> {
        self.provider.listing_request()
    }

    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        ModelsDecoder
    }
}

/// One model as Copilot's `/models` describes it. It deserializes from the
/// listing's own JSON and serializes back to it, so a cached listing
/// reloads as it was fetched.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CopilotModel {
    /// The id requests address.
    pub id: String,
    /// The display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Who makes the model, such as `Anthropic` or `OpenAI`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// The paths the model answers on, such as `/chat/completions`,
    /// `/responses` or `/v1/messages`. Empty when the listing names none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_endpoints: Vec<String>,
    /// What the model takes and how much.
    #[serde(default)]
    pub capabilities: CopilotCapabilities,
}

/// A model's `capabilities` entry.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CopilotCapabilities {
    /// `chat`, `embeddings` or `completion`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    /// The model family the id belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Token limits.
    #[serde(default)]
    pub limits: CopilotLimits,
    /// Supported request features.
    #[serde(default)]
    pub supports: CopilotSupports,
}

/// A model's token limits. Each is `None` when the listing omits it.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotLimits {
    /// Prompt and reply together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_window_tokens: Option<u32>,
    /// The prompt alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_prompt_tokens: Option<u32>,
    /// One streamed reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// One unary reply, when lower than a streamed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_non_streaming_output_tokens: Option<u32>,
}

/// A model's `supports` entry. Each flag is `None` when the listing omits it.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopilotSupports {
    /// The reasoning effort words the model takes, `none` among them when
    /// reasoning turns off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_effort: Vec<String>,
    /// The smallest thinking budget the model takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_thinking_budget: Option<u32>,
    /// The largest thinking budget the model takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_thinking_budget: Option<u32>,
    /// Whether the model takes adaptive thinking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_thinking: Option<bool>,
    /// Whether the model calls tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<bool>,
    /// Whether the model calls several tools in one turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
    /// Whether the model streams.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    /// Whether the model follows a JSON schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_outputs: Option<bool>,
    /// Whether the model reads images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

/// Every effort level, in order.
const EFFORTS: [Effort; 6] = [
    Effort::Minimal,
    Effort::Low,
    Effort::Medium,
    Effort::High,
    Effort::XHigh,
    Effort::Max,
];

impl CopilotModel {
    /// A model named `id` that the listing says nothing else about.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }

    /// Whether the listing names `path` among the model's endpoints.
    pub fn serves(&self, path: &str) -> bool {
        self.supported_endpoints
            .iter()
            .any(|endpoint| endpoint == path)
    }

    /// The endpoint completions for this model go to: Messages when it
    /// answers on `/v1/messages`, else Chat Completions, else Responses.
    /// A model whose listing names no endpoint is routed by its id
    /// ([`CopilotEndpoint::for_id`]).
    pub fn endpoint(&self) -> CopilotEndpoint {
        if self.serves("/v1/messages") {
            CopilotEndpoint::Messages
        } else if self.serves("/chat/completions") {
            CopilotEndpoint::Chat
        } else if self.serves("/responses") {
            CopilotEndpoint::Responses
        } else {
            CopilotEndpoint::for_id(&self.id)
        }
    }

    /// The model's facts: the catalog's Copilot row when it lists the id
    /// (for prices and quirks), overridden by what this listing states. A
    /// reasoning control the listing names replaces the catalog's: its
    /// effort words, its thinking budget range, and whether reasoning turns
    /// off (`none` among the efforts, or Messages, where thinking is opt-in).
    pub fn spec(&self) -> ModelSpec {
        let endpoint = self.endpoint();
        let provider = match endpoint {
            CopilotEndpoint::Messages => {
                ProviderId::of_messages(&crate::providers::anthropic::wire::COPILOT)
            }
            CopilotEndpoint::Chat | CopilotEndpoint::Responses => {
                ProviderId::of_openai(&super::wire::DIALECT)
            }
        };
        let mut spec = Catalog::builtin()
            .get_vendor(super::PROVIDER_NAME, &self.id)
            .map(|resolved| resolved.spec.clone())
            .unwrap_or_else(|| ModelSpec::new(provider, self.id.clone()));
        spec.id = self.id.clone();
        spec.provider = provider;
        if let Some(name) = &self.name {
            spec.display_name = name.clone();
        }
        let capabilities = &self.capabilities;
        let limits = &capabilities.limits;
        let supports = &capabilities.supports;
        if let Some(tokens) = limits.max_context_window_tokens {
            spec.context_window = Some(tokens);
        }
        if let Some(tokens) = limits.max_output_tokens {
            spec.max_output_tokens = Some(tokens);
        }
        if let Some(vision) = supports.vision {
            spec.input.image = vision;
        }
        if let Some(tools) = supports.tool_calls {
            spec.tools = tools;
        }
        if let Some(structured) = supports.structured_outputs {
            spec.structured_output = structured;
        }
        if let Some(adaptive) = supports.adaptive_thinking {
            spec.compat.adaptive_thinking = adaptive;
        }
        if let Some(reasoning) = self.reasoning() {
            spec.reasoning = reasoning;
        }
        spec
    }

    /// The reasoning controls the listing names, or `None` when it names
    /// none.
    fn reasoning(&self) -> Option<ReasoningSupport> {
        let supports = &self.capabilities.supports;
        let budget = match (supports.min_thinking_budget, supports.max_thinking_budget) {
            (Some(min), Some(max)) if min <= max => Some(min..=max),
            _ => None,
        };
        if supports.reasoning_effort.is_empty() && budget.is_none() {
            return None;
        }
        let names = |word: &str| supports.reasoning_effort.iter().any(|name| name == word);
        let levels = EFFORTS
            .into_iter()
            .filter(|effort| names(effort.as_str()))
            .collect();
        Some(ReasoningSupport::Listed {
            levels,
            budget,
            can_disable: names("none") || self.endpoint() == CopilotEndpoint::Messages,
            default: None,
        })
    }

    /// The `max_tokens` a Messages request without one defaults to: the
    /// unary ceiling when the listing gives one, which also bounds a stream,
    /// else the streamed one.
    pub(crate) fn default_max_tokens(&self) -> Option<u64> {
        let limits = &self.capabilities.limits;
        limits
            .max_non_streaming_output_tokens
            .or(limits.max_output_tokens)
            .map(u64::from)
    }
}

/// Lists Copilot's models as typed [`CopilotModel`] entries in one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopilotModelListing;

impl Operation for CopilotModelListing {
    type Request = ();
    type Event = std::convert::Infallible;
    type End = Vec<CopilotModel>;
    type Response = Vec<CopilotModel>;
    type Fold = Whole<Self>;
    type Emit = Free;

    fn fold(_request: &Self::Request, _call: &mut Call<'_>) -> Self::Fold {
        Whole::new()
    }
}

/// Copilot's typed model-listing wire: the same `GET /models` as
/// [`Models`], read into [`CopilotModel`] entries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelDetails {
    /// Which Copilot, and how to reach it.
    pub provider: CopilotConfig,
}

/// The typed model-listing decoder. An entry that does not read as a
/// [`CopilotModel`] (one without an `id`) is skipped.
#[derive(Default)]
pub struct ModelDetailsDecoder;

impl<'id> Decoder<'id, CopilotModelListing> for ModelDetailsDecoder {
    type Event = Value;

    fn classify(&self, frame: WireFrame) -> WireEvent<Self::Event> {
        classify_untyped_line(frame.as_str().as_bytes())
    }

    fn decode(
        &mut self,
        event: Value,
        out: Out<'id, CopilotModelListing>,
    ) -> Result<Flow, ProviderError> {
        let models = event
            .arr("data")
            .iter()
            .filter_map(|entry| serde_json::from_value(entry.clone()).ok())
            .collect();
        Ok(out.end(models))
    }
}

impl Wire for ModelDetails {
    type Op = CopilotModelListing;
    type Payload = crate::wire::Encoded;
    type Frame = crate::wire::WireFrame;
    type Decoder<'id> = ModelDetailsDecoder;
    type Reassembler = crate::wire::document::Unreassembled;

    fn describe(&self) -> Descriptor<'_> {
        Descriptor::new(PROVIDER_NAME)
    }

    fn encode(&self, _request: (), _mode: Mode) -> Result<Encoded, EncodeError> {
        self.provider.listing_request()
    }

    fn decoder<'id>(&self) -> Self::Decoder<'id> {
        ModelDetailsDecoder
    }
}
