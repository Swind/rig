use std::{collections::HashMap, fs, path::Path, sync::Arc};

use qdrant_edge::{
    Distance, EdgeConfig, EdgeShard, EdgeVectorParams, NamedQuery, PointId, PointInsertOperations,
    PointOperations, PointStruct as EdgePointStruct, QueryEnum, QueryRequest, ScoringQuery,
    UpdateOperation, VectorInternal, Vectors, WithPayloadInterface,
};
use rig_core::{
    DynModel,
    embeddings::Embedding,
    vector_store::{
        InsertDocuments, VectorStoreError, VectorStoreIndex,
        request::{Filter, FilterError, VectorSearchRequest},
    },
    wasm_compat::WasmCompatSend,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// Rig vector store backed by one persistent, in-process Qdrant Edge shard.
///
/// The named vector must already exist with the configured dimension and Cosine distance when
/// opening an existing shard. Constructors and operations that access the shard require a Tokio
/// runtime. Edge operations run on its blocking pool. Dropping the last clone synchronously flushes
/// the shard through Qdrant Edge's `Drop` implementation and can panic on I/O failure.
/// Callers must keep only one independently opened store per directory across all processes;
/// use clones to share it. The backend does not enforce exclusive directory ownership.
#[derive(Clone)]
pub struct QdrantEdgeVectorStore {
    model: DynModel<rig_core::operation::Embedding>,
    shard: Arc<EdgeShard>,
    vector_name: String,
    dimensions: usize,
}

impl QdrantEdgeVectorStore {
    /// Creates a new local shard containing one named Cosine vector.
    ///
    /// `path` must be absent or an empty directory. `dimensions` must match the embedding model
    /// when the model reports a nonzero dimension. Existing nonempty directories are rejected.
    /// No other store or process may access the directory while this store or its clones exist.
    /// Call this method from a Tokio runtime because shard creation runs on its blocking pool.
    pub async fn create(
        path: impl AsRef<Path>,
        model: impl Into<DynModel<rig_core::operation::Embedding>>,
        vector_name: impl Into<String>,
        dimensions: usize,
    ) -> Result<Self, VectorStoreError> {
        let model = model.into();
        let vector_name = vector_name.into();
        validate_model_and_schema(&model, &vector_name, dimensions)?;
        let path = path.as_ref().to_path_buf();

        let configured_name = vector_name.clone();
        let shard = tokio::task::spawn_blocking(move || -> Result<_, VectorStoreError> {
            ensure_empty_or_missing(&path).map_err(VectorStoreError::datastore)?;
            fs::create_dir_all(&path).map_err(VectorStoreError::datastore)?;
            let config = EdgeConfig {
                vectors: HashMap::from([(
                    configured_name,
                    EdgeVectorParams {
                        size: dimensions,
                        distance: Distance::Cosine,
                        on_disk: None,
                        multivector_config: None,
                        datatype: None,
                        quantization_config: None,
                        hnsw_config: None,
                    },
                )]),
                ..Default::default()
            };
            EdgeShard::new(&path, config).map_err(VectorStoreError::datastore)
        })
        .await
        .map_err(VectorStoreError::datastore)??;

        Ok(Self {
            model,
            shard: Arc::new(shard),
            vector_name,
            dimensions,
        })
    }

    /// Opens a shard created with a compatible named Cosine vector.
    ///
    /// The persisted configuration is checked before Edge loads the shard, so a mismatch or
    /// missing configuration does not rewrite the directory.
    /// No other store or process may access the directory while this store or its clones exist.
    /// Call this method from a Tokio runtime because shard loading runs on its blocking pool.
    pub async fn open(
        path: impl AsRef<Path>,
        model: impl Into<DynModel<rig_core::operation::Embedding>>,
        vector_name: impl Into<String>,
        dimensions: usize,
    ) -> Result<Self, VectorStoreError> {
        let model = model.into();
        let vector_name = vector_name.into();
        validate_model_and_schema(&model, &vector_name, dimensions)?;
        let path = path.as_ref().to_path_buf();
        let expected_name = vector_name.clone();

        let shard = tokio::task::spawn_blocking(move || -> Result<_, VectorStoreError> {
            let config_path = path.join("edge_config.json");
            let config_file = fs::File::open(config_path).map_err(VectorStoreError::datastore)?;
            let config: EdgeConfig =
                serde_json::from_reader(config_file).map_err(VectorStoreError::datastore)?;
            validate_config(&config, &expected_name, dimensions)?;
            EdgeShard::load(&path, None).map_err(VectorStoreError::datastore)
        })
        .await
        .map_err(VectorStoreError::datastore)??;

        Ok(Self {
            model,
            shard: Arc::new(shard),
            vector_name,
            dimensions,
        })
    }

    /// Flushes the shard on Tokio's blocking pool.
    ///
    /// Qdrant Edge 0.6.1 exposes a synchronous, infallible `flush` method that panics on an
    /// underlying flush failure. This method reports blocking-task failures; it cannot recover
    /// backend I/O errors hidden by that API.
    pub async fn flush(&self) -> Result<(), VectorStoreError> {
        let shard = Arc::clone(&self.shard);
        tokio::task::spawn_blocking(move || shard.flush())
            .await
            .map_err(VectorStoreError::datastore)
    }

    /// Runs Edge's synchronous optimizer on Tokio's blocking pool.
    pub async fn optimize(&self) -> Result<bool, VectorStoreError> {
        let shard = Arc::clone(&self.shard);
        tokio::task::spawn_blocking(move || shard.optimize())
            .await
            .map_err(VectorStoreError::datastore)?
            .map_err(VectorStoreError::datastore)
    }

    async fn query_vector(&self, query: &str) -> Result<Vec<f32>, VectorStoreError> {
        let embedding = self.model.embed_text(query).await?;
        validate_embedding(&embedding, self.dimensions)
    }

    async fn search(
        &self,
        request: VectorSearchRequest<Filter<Value>>,
        with_payload: bool,
    ) -> Result<Vec<qdrant_edge::ScoredPoint>, VectorStoreError> {
        let query_text = request.query().trim();
        if query_text.is_empty() {
            return Err(VectorStoreError::BuilderError(
                "query must not be empty".to_owned(),
            ));
        }
        let limit = usize::try_from(request.samples()).map_err(|_| {
            VectorStoreError::BuilderError("sample count exceeds this platform's limit".to_owned())
        })?;
        if limit == 0 {
            return Err(VectorStoreError::BuilderError(
                "sample count must be greater than zero".to_owned(),
            ));
        }
        let threshold = request
            .threshold()
            .map(|threshold| {
                let threshold_f32 = threshold as f32;
                if !threshold.is_finite() || !threshold_f32.is_finite() {
                    return Err(VectorStoreError::BuilderError(
                        "score threshold must be finite and representable as f32".to_owned(),
                    ));
                }
                Ok(qdrant_edge::external::ordered_float::OrderedFloat(
                    threshold_f32,
                ))
            })
            .transpose()?;

        let filter = request.filter().as_ref().map(filter_to_edge).transpose()?;
        let vector = self.query_vector(query_text).await?;
        let query = QueryRequest {
            prefetches: Vec::new(),
            query: Some(ScoringQuery::Vector(QueryEnum::Nearest(NamedQuery::new(
                VectorInternal::from(vector),
                self.vector_name.clone(),
            )))),
            filter,
            score_threshold: threshold,
            limit,
            offset: 0,
            params: None,
            with_vector: qdrant_edge::WithVector::Bool(false),
            with_payload: WithPayloadInterface::Bool(with_payload),
        };

        let shard = Arc::clone(&self.shard);
        tokio::task::spawn_blocking(move || shard.query(query))
            .await
            .map_err(VectorStoreError::datastore)?
            .map_err(VectorStoreError::datastore)
    }
}

impl InsertDocuments for QdrantEdgeVectorStore {
    async fn insert_documents<Doc: Serialize + rig_core::Embed + WasmCompatSend>(
        &self,
        documents: Vec<(Doc, Vec<Embedding>)>,
    ) -> Result<(), VectorStoreError> {
        let mut points = Vec::new();
        for (document, embeddings) in documents {
            if embeddings.is_empty() {
                return Err(VectorStoreError::BuilderError(
                    "each document must contain at least one embedding".to_owned(),
                ));
            }
            let payload = serde_json::to_value(document)?;
            if !payload.is_object() {
                return Err(VectorStoreError::BuilderError(
                    "Qdrant Edge payloads must serialize as JSON objects".to_owned(),
                ));
            }
            for embedding in embeddings {
                let vector = validate_embedding(&embedding, self.dimensions)?;
                let point = EdgePointStruct::new(
                    PointId::Uuid(qdrant_edge::external::uuid::Uuid::new_v4()),
                    Vectors::new_named([(self.vector_name.as_str(), vector)]),
                    payload.clone(),
                );
                points.push(point.into());
            }
        }

        if points.is_empty() {
            return Ok(());
        }

        let update = UpdateOperation::PointOperation(PointOperations::UpsertPoints(
            PointInsertOperations::PointsList(points),
        ));
        let shard = Arc::clone(&self.shard);
        tokio::task::spawn_blocking(move || shard.update(update))
            .await
            .map_err(VectorStoreError::datastore)?
            .map_err(VectorStoreError::datastore)
    }
}

impl VectorStoreIndex for QdrantEdgeVectorStore {
    type Filter = Filter<Value>;

    async fn top_n<T: DeserializeOwned + WasmCompatSend>(
        &self,
        request: VectorSearchRequest<Self::Filter>,
    ) -> Result<Vec<(f64, String, T)>, VectorStoreError> {
        self.search(request, true)
            .await?
            .into_iter()
            .map(|point| {
                let id = stringify_id(point.id);
                let payload = point.payload.ok_or_else(|| {
                    VectorStoreError::BuilderError("search result has no payload".to_owned())
                })?;
                let payload = serde_json::to_value(payload)?;
                Ok((point.score as f64, id, serde_json::from_value(payload)?))
            })
            .collect()
    }

    async fn top_n_ids(
        &self,
        request: VectorSearchRequest<Self::Filter>,
    ) -> Result<Vec<(f64, String)>, VectorStoreError> {
        Ok(self
            .search(request, false)
            .await?
            .into_iter()
            .map(|point| (point.score as f64, stringify_id(point.id)))
            .collect())
    }
}

fn validate_model_and_schema(
    model: &DynModel<rig_core::operation::Embedding>,
    vector_name: &str,
    dimensions: usize,
) -> Result<(), VectorStoreError> {
    if vector_name.is_empty() {
        return Err(VectorStoreError::BuilderError(
            "vector name must not be empty".to_owned(),
        ));
    }
    if dimensions == 0 {
        return Err(VectorStoreError::BuilderError(
            "vector dimensions must be greater than zero".to_owned(),
        ));
    }
    let model_dimensions = model.capabilities().ndims;
    if model_dimensions != 0 && model_dimensions != dimensions {
        return Err(VectorStoreError::BuilderError(format!(
            "embedding model reports {model_dimensions} dimensions, but the shard is configured for {dimensions}"
        )));
    }
    Ok(())
}

fn validate_config(
    config: &EdgeConfig,
    vector_name: &str,
    dimensions: usize,
) -> Result<(), VectorStoreError> {
    let params = config.vectors.get(vector_name).ok_or_else(|| {
        VectorStoreError::BuilderError(format!(
            "Qdrant Edge shard has no vector named {vector_name:?}"
        ))
    })?;
    if params.size != dimensions || params.distance != Distance::Cosine {
        return Err(VectorStoreError::BuilderError(format!(
            "Qdrant Edge vector {vector_name:?} has dimension {} and distance {:?}; expected dimension {dimensions} and Cosine",
            params.size, params.distance
        )));
    }
    Ok(())
}

fn validate_embedding(
    embedding: &Embedding,
    dimensions: usize,
) -> Result<Vec<f32>, VectorStoreError> {
    if embedding.vec.len() != dimensions {
        return Err(VectorStoreError::BuilderError(format!(
            "embedding has {} dimensions, expected {dimensions}",
            embedding.vec.len()
        )));
    }
    let vector = embedding
        .vec
        .iter()
        .map(|value| *value as f32)
        .collect::<Vec<_>>();
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(VectorStoreError::BuilderError(
            "embedding values must be finite and representable as f32".to_owned(),
        ));
    }
    if vector.iter().all(|value| *value == 0.0) {
        return Err(VectorStoreError::BuilderError(
            "Cosine embeddings must not be zero vectors".to_owned(),
        ));
    }
    Ok(vector)
}

fn ensure_empty_or_missing(path: &Path) -> std::io::Result<()> {
    if path.exists() && fs::read_dir(path)?.next().is_some() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "Qdrant Edge create requires an absent or empty directory",
        ));
    }
    Ok(())
}

fn stringify_id(id: PointId) -> String {
    match id {
        PointId::NumId(value) => value.to_string(),
        PointId::Uuid(value) => value.to_string(),
    }
}

fn filter_to_edge(filter: &Filter<Value>) -> Result<qdrant_edge::Filter, FilterError> {
    fn condition(filter: &Filter<Value>) -> Result<Value, FilterError> {
        match filter {
            Filter::Eq(key, value) => {
                let supported = match value {
                    Value::String(_) | Value::Bool(_) => true,
                    Value::Number(number) => number.as_i64().is_some(),
                    _ => false,
                };
                if !supported {
                    return Err(FilterError::TypeError(format!(
                        "Qdrant Edge equality filters support strings, booleans, and signed integers; got {value}"
                    )));
                }
                Ok(json!({ "key": key, "match": { "value": value } }))
            }
            Filter::Gt(key, value) | Filter::Lt(key, value) => {
                let number = value
                    .as_f64()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| {
                        FilterError::TypeError(format!(
                            "Qdrant Edge range filters require a finite number; got {value}"
                        ))
                    })?;
                let range = if matches!(filter, Filter::Gt(_, _)) {
                    json!({ "gt": number })
                } else {
                    json!({ "lt": number })
                };
                Ok(json!({ "key": key, "range": range }))
            }
            Filter::And(left, right) => Ok(json!({
                "must": [condition(left)?, condition(right)?]
            })),
            Filter::Or(left, right) => Ok(json!({
                "should": [condition(left)?, condition(right)?]
            })),
        }
    }

    serde_json::from_value(json!({ "must": [condition(filter)?] }))
        .map_err(|error| FilterError::Serialization(error.to_string()))
}
