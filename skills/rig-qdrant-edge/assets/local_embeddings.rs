//! Tiny deterministic embedding model for the offline Qdrant Edge example and tests.
//!
//! This maps a few words onto fixed axes so retrieval behavior is easy to see.
//! It is a fixture, not a useful semantic embedding model.

use futures::stream;
use rig_core::{
    driver::{Exchange, Local, Model, Opened, Opening, Step, Transport},
    embeddings::{Embedding, EmbeddingResponse},
    error::ProviderError,
    operation::Embedding as EmbeddingOp,
    wire::Capabilities,
};

/// A deterministic, local three-dimensional embedding model for examples and tests.
pub type LocalEmbeddingModel = Model<Local<EmbeddingOp>, LocalEmbeddingTransport>;

/// Runtime for [`LocalEmbeddingModel`].
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalEmbeddingTransport;

/// Build the deterministic local embedding model.
pub fn local_embedding_model() -> LocalEmbeddingModel {
    local_embedding_model_with_dimensions(3)
}

/// Build the fixture model with a declared width, useful for schema mismatch tests.
pub fn local_embedding_model_with_dimensions(dimensions: usize) -> LocalEmbeddingModel {
    Model::new(
        Local::new("rig-qdrant-edge-fixture")
            .with_capabilities(Capabilities::embedding(64, dimensions)),
        LocalEmbeddingTransport,
    )
}

impl Transport<Local<EmbeddingOp>> for LocalEmbeddingTransport {
    fn send(&self, texts: Vec<String>, _exchange: Exchange) -> Opening<Step<EmbeddingOp>> {
        if texts.iter().any(|text| text.contains("model-error")) {
            return Opening::failed(ProviderError::Provider(
                "fixture embedding failure".to_owned(),
            ));
        }

        let response = EmbeddingResponse {
            provider: "rig-qdrant-edge-fixture".to_owned(),
            ..EmbeddingResponse::new(
                texts
                    .into_iter()
                    .map(|document| Embedding {
                        vec: direction(&document),
                        document,
                    })
                    .collect(),
            )
        };

        Opening::ready(Opened::new(stream::iter([Ok::<_, ProviderError>(
            Step::End(response),
        )])))
    }
}

fn direction(text: &str) -> Vec<f64> {
    let text = text.to_ascii_lowercase();
    if text.contains("wrong-dimension") {
        vec![1.0, 0.0]
    } else if text.contains("qdrant") || text.contains("database") {
        vec![1.0, 0.0, 0.0]
    } else if text.contains("cooking") || text.contains("recipe") {
        vec![0.0, 1.0, 0.0]
    } else {
        vec![0.0, 0.0, 1.0]
    }
}
