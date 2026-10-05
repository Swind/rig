use super::*;
use anyhow::ensure;
use serde_json::json;

#[tokio::test]
async fn parameters_rows_and_reopen() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("graph.db");
    let client = LadybugClient::open_with_config(&path, test_config())?;
    client
        .query(CypherRequest::new(
            "CREATE NODE TABLE Person(id STRING, PRIMARY KEY(id))",
        ))
        .await?;
    let name = "O'Reilly 東京'); MATCH (n) DELETE n; //";
    client
        .query(CypherRequest::new("CREATE (:Person {id: $name})").param("name", json!(name)))
        .await?;
    let rows = client
        .query(CypherRequest::new(
            "MATCH (p:Person) RETURN p.id AS name ORDER BY p.id",
        ))
        .await?;
    ensure!(rows.len() == 1 && rows.first().and_then(|row| row.get("name")) == Some(&json!(name)));
    let payload = json!({"numbers": [1, null, 3], "nested": {"yes": true, "text": "東京"}});
    let rows = client
        .query(
            CypherRequest::new("RETURN $payload AS payload, null AS absent")
                .param("payload", payload.clone()),
        )
        .await?;
    ensure!(
        rows.len() == 1
            && rows.first().and_then(|row| row.get("payload")) == Some(&payload)
            && rows.first().and_then(|row| row.get("absent")) == Some(&Value::Null)
    );
    drop(client);
    let reopened = LadybugClient::open_with_config(path, test_config())?;
    let rows = reopened
        .query(CypherRequest::new(
            "MATCH (p:Person) RETURN count(*) AS count",
        ))
        .await?;
    ensure!(rows.first().and_then(|row| row.get("count")) == Some(&json!(1)));
    Ok(())
}

#[tokio::test]
async fn transactions_commit_and_rollback() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let client = LadybugClient::open_with_config(directory.path().join("graph.db"), test_config())?;
    client
        .query(CypherRequest::new(
            "CREATE NODE TABLE Entry(id STRING, PRIMARY KEY(id))",
        ))
        .await?;
    client
        .transaction(vec![
            CypherRequest::new("CREATE (:Entry {id: $id})").param("id", json!("kept")),
        ])
        .await?;
    let outcome = client
        .transaction(vec![
            CypherRequest::new("CREATE (:Entry {id: $id})").param("id", json!("rolled back")),
            CypherRequest::new("THIS IS NOT CYPHER"),
        ])
        .await;
    ensure!(matches!(outcome, Err(CypherQueryError::Query { .. })));
    let rows = client
        .query(CypherRequest::new(
            "MATCH (n:Entry) RETURN n.id AS id ORDER BY n.id",
        ))
        .await?;
    ensure!(rows.len() == 1 && rows.first().and_then(|row| row.get("id")) == Some(&json!("kept")));
    let outcome = client
        .transaction(vec![
            CypherRequest::new("CREATE (:Entry {id: 'conversion rollback'})"),
            CypherRequest::new("MATCH (n:Entry) RETURN n AS node"),
        ])
        .await;
    ensure!(matches!(outcome, Err(CypherQueryError::Result { .. })));
    let rows = client
        .query(CypherRequest::new(
            "MATCH (n:Entry) RETURN count(*) AS count",
        ))
        .await?;
    ensure!(rows.first().and_then(|row| row.get("count")) == Some(&json!(1)));
    Ok(())
}

#[tokio::test]
async fn unsupported_parameters_and_results_are_errors() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let client = LadybugClient::open_with_config(directory.path().join("graph.db"), test_config())?;
    for value in [
        json!(u64::MAX),
        json!([1, "mixed"]),
        json!({"overflow": u64::MAX}),
    ] {
        let outcome = client
            .query(CypherRequest::new("RETURN $value AS value").param("value", value))
            .await;
        ensure!(
            matches!(outcome, Err(CypherQueryError::Parameter { name, .. }) if name == "value")
        );
    }
    for statement in [
        "RETURN DATE('2025-01-01') AS value",
        "RETURN {nested: DATE('2025-01-01')} AS value",
    ] {
        ensure!(matches!(
            client.query(CypherRequest::new(statement)).await,
            Err(CypherQueryError::Result { .. })
        ));
    }
    ensure!(matches!(
        to_json(NativeValue::Double(f64::NAN)),
        Err(ProjectionError::NonFinite)
    ));
    ensure!(matches!(
        to_json(NativeValue::UInt64(u64::MAX)),
        Err(ProjectionError::IntegerRange)
    ));
    Ok(())
}

fn test_config() -> SystemConfig {
    SystemConfig::default()
        .buffer_pool_size(64 * 1024 * 1024)
        .max_num_threads(2)
}
