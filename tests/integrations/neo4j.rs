use serde_json::json;
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

use crate::common::{WORD_DEFINITIONS, mock_embeddings, openai_client, skip_if_docker_unavailable};
use futures::{StreamExt, TryStreamExt};
use rig::cypher::{CypherQuery, CypherQueryError, CypherRequest, CypherRow};
use rig::neo4j::{Neo4jClient, ToBoltType};
use rig::vector_store::VectorStoreIndex;
use rig::vector_store::request::VectorSearchRequest;
use rig::{
    Embed,
    driver::Model,
    embeddings::{Embedding, EmbeddingsBuilder},
    providers::openai,
};

const BOLT_PORT: u16 = 7687;
const HTTP_PORT: u16 = 7474;

#[derive(Embed, Clone, serde::Deserialize, Debug)]
struct Word {
    id: String,
    #[embed]
    definition: String,
}

async fn cypher_query(
    client: &impl CypherQuery,
    request: CypherRequest,
) -> Result<Vec<CypherRow>, CypherQueryError> {
    client.query(request).await
}

#[tokio::test]
async fn cypher_query_contract_test() -> anyhow::Result<()> {
    use std::error::Error as _;

    if skip_if_docker_unavailable("cypher_query_contract_test") {
        return Ok(());
    }

    let container = GenericImage::new("neo4j", "5.26.29")
        .with_wait_for(WaitFor::Duration {
            length: std::time::Duration::from_secs(5),
        })
        .with_exposed_port(BOLT_PORT.tcp())
        .with_env_var("NEO4J_AUTH", "none")
        .start()
        .await?;
    let port = container.get_host_port_ipv4(BOLT_PORT).await?;
    let host = container.get_host().await?;
    let client = Neo4jClient::connect(&format!("neo4j://{host}:{port}"), "", "").await?;

    let name = "O'Reilly\\東京'); MATCH (n) DETACH DELETE n; //";
    cypher_query(
        &client,
        CypherRequest::new(
            "CREATE (p:CypherPerson {name: $name}),
                    (a:CypherCompany {name: 'Acme'}),
                    (b:CypherCompany {name: 'Beta'}),
                    (p)-[:WORKS_AT]->(a), (p)-[:WORKS_AT]->(b)
             RETURN p.name AS person",
        )
        .param("name", json!(name)),
    )
    .await?;

    let rows = cypher_query(
        &client,
        CypherRequest::new(
            "MATCH (p:CypherPerson)-[:WORKS_AT]->(c:CypherCompany)
             WHERE p.name = $name
             RETURN p.name AS person, c.name AS company ORDER BY company",
        )
        .param("name", json!(name)),
    )
    .await?;
    anyhow::ensure!(
        serde_json::to_value(rows)?
            == json!([
                {"person": name, "company": "Acme"},
                {"person": name, "company": "Beta"}
            ]),
        "parameter binding or ordered relationship projection changed"
    );

    let payload = json!({
        "null": null,
        "bool": true,
        "integer": -42,
        "float": 1.25,
        "string": name,
        "list": [null, false, 7, {"nested": ["文字", 2.5]}],
        "map": {"value": 0}
    });
    let rows = cypher_query(
        &client,
        CypherRequest::new("RETURN $payload AS payload").param("payload", payload.clone()),
    )
    .await?;
    anyhow::ensure!(
        serde_json::to_value(rows)? == json!([{"payload": payload}]),
        "JSON values or the single-column map alias were lost"
    );
    let rows = cypher_query(&client, CypherRequest::new("RETURN null AS absent")).await?;
    anyhow::ensure!(serde_json::to_value(rows)? == json!([{"absent": null}]));
    let rows = cypher_query(
        &client,
        CypherRequest::new("MATCH (p:CypherPerson) WHERE false RETURN p.name AS name"),
    )
    .await?;
    anyhow::ensure!(rows.is_empty(), "an empty result should have no rows");

    let result = cypher_query(&client, CypherRequest::new("THIS IS NOT CYPHER")).await;
    anyhow::ensure!(
        matches!(&result, Err(CypherQueryError::Query { .. })),
        "invalid Cypher should return a query error: {result:?}"
    );
    if let Err(error) = result {
        anyhow::ensure!(error.source().is_some(), "query error lost its source");
    }

    let result = cypher_query(
        &client,
        CypherRequest::new("CREATE (:RejectedParameter {value: $value}) RETURN 1 AS created")
            .param("value", json!(u64::MAX)),
    )
    .await;
    anyhow::ensure!(
        matches!(&result, Err(CypherQueryError::Parameter { name, .. }) if name == "value"),
        "an oversized integer should return a named parameter error: {result:?}"
    );
    if let Err(error) = result {
        anyhow::ensure!(error.source().is_some(), "parameter error lost its source");
    }
    let rows = cypher_query(
        &client,
        CypherRequest::new("MATCH (n:RejectedParameter) RETURN count(n) AS count"),
    )
    .await?;
    anyhow::ensure!(
        serde_json::to_value(rows)? == json!([{"count": 0}]),
        "invalid parameters must be rejected before executing the statement"
    );

    for statement in [
        "RETURN date('2026-01-01') AS day",
        "RETURN duration('P1D') AS elapsed",
        "RETURN {elapsed: [duration('P1D')]} AS nested",
        "MATCH (p:CypherPerson) RETURN p AS person",
        "MATCH (p:CypherPerson) RETURN {people: [p]} AS nested",
    ] {
        let result = cypher_query(&client, CypherRequest::new(statement)).await;
        anyhow::ensure!(
            matches!(&result, Err(CypherQueryError::Result { .. })),
            "unsupported native values should return a result error: {result:?}"
        );
        if let Err(error) = result {
            anyhow::ensure!(error.source().is_some(), "result error lost its source");
        }
    }
    Ok(())
}

#[tokio::test]
async fn vector_search_test() {
    if skip_if_docker_unavailable("vector_search_test") {
        return;
    }

    // Setup a local Neo 4J container for testing. NOTE: docker service must be running.
    // Pinned like `pgvector:pg17` / `scylla:5.4`: a floating `latest` defeats
    // layer caching and lets a rerun silently test a different database version.
    let container = GenericImage::new("neo4j", "5.26.29")
        .with_wait_for(WaitFor::Duration {
            length: std::time::Duration::from_secs(5),
        })
        .with_exposed_port(BOLT_PORT.tcp())
        .with_exposed_port(HTTP_PORT.tcp())
        .with_env_var("NEO4J_AUTH", "none")
        .start()
        .await
        .expect("Failed to start Neo 4J container");

    let port = container.get_host_port_ipv4(BOLT_PORT).await.expect("");
    let host = container.get_host().await.expect("").to_string();

    let neo4j_client = Neo4jClient::connect(&format!("neo4j://{host}:{port}"), "", "")
        .await
        .expect("");

    // Setup mock openai API
    let server = httpmock::MockServer::start();

    mock_embeddings(
        &server,
        json!({ "input": WORD_DEFINITIONS, "model": "text-embedding-ada-002" }),
        [
            vec![-0.001; 1536],
            vec![0.0023064255; 1536],
            vec![-0.001; 1536],
        ],
    );
    mock_embeddings(
        &server,
        json!({ "input": ["What is a glarb?"], "model": "text-embedding-ada-002" }),
        [vec![0.0024064254; 1536]],
    );

    let openai_client = openai_client(&server);

    // Select the embedding model and generate our embeddings
    let model = openai_client.embedding(openai::TEXT_EMBEDDING_ADA_002, None);

    let embeddings = create_embeddings(model.clone()).await;

    futures::stream::iter(embeddings)
        .map(|(doc, embeddings)| {
            let embedding = embeddings.first().expect("expected at least one embedding");
            neo4j_client.graph.run(
                neo4rs::query(
                    "
                        CREATE
                            (document:DocumentEmbeddings {
                                id: $id,
                                document: $document,
                                embedding: $embedding})
                        RETURN document",
                )
                .param("id", doc.id)
                // Here we use the first embedding but we could use any of them.
                // Neo4j only takes primitive types or arrays as properties.
                .param("embedding", embedding.vec.clone())
                .param("document", doc.definition.to_bolt_type()),
            )
        })
        .buffer_unordered(3)
        .try_collect::<Vec<_>>()
        .await
        .expect("");

    // Create a vector index on our vector store
    println!("Creating vector index...");
    neo4j_client
        .graph
        .run(neo4rs::query(
            "CREATE VECTOR INDEX vector_index IF NOT EXISTS
                FOR (m:DocumentEmbeddings)
                ON m.embedding
                OPTIONS { indexConfig: {
                    `vector.dimensions`: 1536,
                    `vector.similarity_function`: 'cosine'
                    }}",
        ))
        .await
        .expect("");

    // ℹ️ The index name must be unique among both indexes and constraints.
    // A newly created index is not immediately available but is created in the background.

    // Check if the index exists with db.awaitIndex(), the call timeouts if the index is not ready
    let index_exists = neo4j_client
        .graph
        .run(neo4rs::query("CALL db.awaitIndex('vector_index')"))
        .await;
    if index_exists.is_err() {
        println!("Index not ready, waiting for index...");
        std::thread::sleep(std::time::Duration::from_secs(5));
    }

    println!("Index exists: {index_exists:?}");

    // Create a vector index on our vector store
    // IMPORTANT: Reuse the same model that was used to generate the embeddings
    let index = neo4j_client
        .get_index(model, "vector_index")
        .await
        .expect("");

    let query = "What is a glarb?";
    let req = VectorSearchRequest::builder()
        .query(query)
        .samples(1)
        .build();

    // Query the index
    let results = index.top_n::<serde_json::Value>(req).await.expect("");

    let value = &results.first().expect("").document;

    assert_eq!(
        value,
        &serde_json::json!({
            "id": "doc1",
            "document": "Definition of a *glarb-glarb*: A glarb-glarb is an ancient tool used by the ancestors of the inhabitants of planet Jiro to farm the land.",
            "embedding": serde_json::Value::Null
        })
    );
}

async fn create_embeddings(model: Model<openai::wire::Embeddings>) -> Vec<(Word, Vec<Embedding>)> {
    let words = WORD_DEFINITIONS
        .iter()
        .enumerate()
        .map(|(i, definition)| Word {
            id: format!("doc{i}"),
            definition: definition.to_string(),
        });

    EmbeddingsBuilder::new(model)
        .documents(words)
        .expect("")
        .build()
        .await
        .expect("")
}
