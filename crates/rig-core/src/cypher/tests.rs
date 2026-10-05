use std::io;

use serde_json::json;

use super::*;

#[test]
fn parameters_are_separate_and_rebinding_replaces_the_value() {
    let statement = "RETURN $name AS name";
    let name = "Alice's \"公司\"";
    let request = CypherRequest::new(statement)
        .param("name", json!("old"))
        .param("name", json!(name));

    assert_eq!(request.statement, statement);
    assert_eq!(
        request.parameters,
        BTreeMap::from([("name".into(), json!(name))])
    );
}

struct MockCypher;

impl CypherQuery for MockCypher {
    async fn query(&self, request: CypherRequest) -> Result<Vec<CypherRow>, CypherQueryError> {
        if request.statement != "RETURN $name AS name"
            || request.parameters != BTreeMap::from([("name".into(), json!("Alice"))])
        {
            return Err(CypherQueryError::query(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mock expected a named Alice parameter and a projected name column",
            )));
        }
        Ok(vec![BTreeMap::from([("name".into(), json!("Alice"))])])
    }
}

#[tokio::test]
async fn generic_consumer_uses_named_parameters_and_projected_rows() -> anyhow::Result<()> {
    async fn find_name(database: &impl CypherQuery) -> Result<Vec<CypherRow>, CypherQueryError> {
        database
            .query(CypherRequest::new("RETURN $name AS name").param("name", json!("Alice")))
            .await
    }

    let rows = find_name(&MockCypher).await?;
    anyhow::ensure!(
        rows == vec![BTreeMap::from([("name".into(), json!("Alice"))])],
        "generic consumer must preserve projected row values"
    );
    Ok(())
}

#[test]
fn errors_preserve_their_source_and_phase() {
    let parameter = CypherQueryError::parameter("name", io::Error::other("unsupported value"));
    assert!(matches!(&parameter, CypherQueryError::Parameter { name, .. } if name == "name"));
    let query = CypherQueryError::query(io::Error::other("invalid statement"));
    assert!(matches!(&query, CypherQueryError::Query { .. }));
    let result = CypherQueryError::result(io::Error::other("unsupported column"));
    assert!(matches!(&result, CypherQueryError::Result { .. }));

    for (error, message) in [
        (parameter, "unsupported value"),
        (query, "invalid statement"),
        (result, "unsupported column"),
    ] {
        let Some(source) = error.source() else {
            panic!("Cypher error must preserve its source");
        };
        assert!(source.downcast_ref::<io::Error>().is_some());
        assert_eq!(source.to_string(), message);
    }
}
