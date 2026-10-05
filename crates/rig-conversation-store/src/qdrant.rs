use qdrant_client::{
    Payload, Qdrant,
    qdrant::{
        Condition, CreateCollectionBuilder, CreateFieldIndexCollectionBuilder, DeletePointsBuilder,
        Distance, FieldType, Filter, PointStruct, QueryPointsBuilder, UpsertPointsBuilder,
        VectorParamsBuilder, point_id::PointIdOptions, vectors_config::Config,
    },
};
use rig_core::id::ConversationId;
use serde_json::json;

use crate::{StoreError, storage::Chunk};

pub(crate) struct VectorProjection {
    client: Qdrant,
    collection: String,
}

impl VectorProjection {
    pub(crate) async fn open(
        client: Qdrant,
        collection: String,
        dimension: u64,
    ) -> Result<Self, StoreError> {
        if !client.collection_exists(&collection).await? {
            client
                .create_collection(
                    CreateCollectionBuilder::new(&collection)
                        .vectors_config(VectorParamsBuilder::new(dimension, Distance::Cosine)),
                )
                .await?;
        }
        let info = client
            .collection_info(&collection)
            .await?
            .result
            .and_then(|info| info.config)
            .and_then(|config| config.params)
            .and_then(|params| params.vectors_config)
            .and_then(|vectors| vectors.config);
        match info {
            Some(Config::Params(params)) if params.size == dimension && params.distance == Distance::Cosine as i32 => {},
            _ => return Err(StoreError::Configuration("Qdrant collection must use one unnamed cosine vector with the configured dimension".into())),
        }
        for field in ["scope", "conversation_id"] {
            client
                .create_field_index(
                    CreateFieldIndexCollectionBuilder::new(&collection, field, FieldType::Keyword)
                        .wait(true),
                )
                .await?;
        }
        Ok(Self { client, collection })
    }

    pub(crate) async fn upsert(&self, chunk: &Chunk, vector: Vec<f32>) -> Result<(), StoreError> {
        let payload = Payload::try_from(json!({
            "scope": chunk.scope, "conversation_id": chunk.conversation_id,
            "generation": chunk.generation, "chunk_id": chunk.id,
            "start": chunk.start, "end": chunk.end,
        }))?;
        self.client
            .upsert_points(
                UpsertPointsBuilder::new(
                    &self.collection,
                    vec![PointStruct::new(chunk.id.clone(), vector, payload)],
                )
                .wait(true),
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, chunk: &Chunk) -> Result<(), StoreError> {
        self.client
            .delete_points(
                DeletePointsBuilder::new(&self.collection)
                    .points(Filter::must([
                        Condition::matches("scope", chunk.scope.clone()),
                        Condition::matches("chunk_id", chunk.id.clone()),
                    ]))
                    .wait(true),
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn query(
        &self,
        scope: &str,
        conversation_id: Option<&ConversationId>,
        vector: Vec<f32>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let mut conditions = vec![Condition::matches("scope", scope.to_owned())];
        if let Some(id) = conversation_id {
            conditions.push(Condition::matches("conversation_id", id.to_string()));
        }
        self.client
            .query(
                QueryPointsBuilder::new(&self.collection)
                    .query(vector)
                    .filter(Filter::must(conditions))
                    .offset(offset as u64)
                    .limit(limit as u64)
                    .with_payload(false),
            )
            .await?
            .result
            .into_iter()
            .map(|point| match point.id.and_then(|id| id.point_id_options) {
                Some(PointIdOptions::Uuid(id)) => Ok(id),
                _ => Err(StoreError::Configuration(
                    "Qdrant conversation point is missing its persisted UUID".into(),
                )),
            })
            .collect()
    }
}
