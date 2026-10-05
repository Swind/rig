use neo4rs::{BoltFloat, BoltInteger, BoltList, BoltNode, BoltPath, BoltPoint2D, BoltRelation};
use serde_json::json;

use super::*;

fn row(value: BoltType) -> Row {
    Row::new(
        vec![BoltType::from("projection")].into(),
        vec![value].into(),
    )
}

#[test]
fn projected_json_round_trips_with_column_alias() -> anyhow::Result<()> {
    let value = json!({
        "text": "O'Reilly 台灣",
        "values": [null, true, -1, 1.5, {"nested": [i64::MIN, i64::MAX]}]
    });
    let result = to_row(row(to_bolt(value.clone())?))?;
    anyhow::ensure!(result == BTreeMap::from([("projection".into(), value)]));
    Ok(())
}

#[test]
fn integer_overflow_is_rejected_in_nested_parameters() {
    for value in [
        json!(u64::MAX),
        json!([u64::MAX]),
        json!({"nested": u64::MAX}),
    ] {
        assert!(matches!(
            to_bolt(value),
            Err(ConversionError::IntegerOutOfRange)
        ));
    }
}

#[test]
fn scalar_columns_and_null_keep_names() -> anyhow::Result<()> {
    let row = Row::new(
        vec!["name".into(), "missing".into(), "count".into()].into(),
        vec!["Alice".into(), BoltType::Null(BoltNull), i64::MAX.into()].into(),
    );
    anyhow::ensure!(
        to_row(row)?
            == BTreeMap::from([
                ("name".into(), json!("Alice")),
                ("missing".into(), Value::Null),
                ("count".into(), json!(i64::MAX)),
            ])
    );
    Ok(())
}

#[test]
fn non_finite_floats_are_rejected_in_nested_results() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let scalar = BoltType::Float(BoltFloat::new(value));
        assert!(matches!(
            to_row(row(scalar.clone())),
            Err(CypherQueryError::Result { .. })
        ));
        assert!(matches!(
            to_row(row(BoltType::List(vec![scalar].into()))),
            Err(CypherQueryError::Result { .. })
        ));
    }
}

#[test]
fn native_graph_and_spatial_values_are_rejected_recursively() {
    let values = [
        BoltType::Duration(neo4rs::BoltDuration::new(
            1.into(),
            2.into(),
            3.into(),
            4.into(),
        )),
        BoltType::Node(BoltNode::new(
            BoltInteger::new(1),
            BoltList::new(),
            BoltMap::new(),
        )),
        BoltType::Relation(BoltRelation {
            id: BoltInteger::new(1),
            start_node_id: BoltInteger::new(1),
            end_node_id: BoltInteger::new(2),
            typ: BoltString::new("KNOWS"),
            properties: BoltMap::new(),
        }),
        BoltType::Path(BoltPath {
            nodes: BoltList::new(),
            rels: BoltList::new(),
            indices: BoltList::new(),
        }),
        BoltType::Point2D(BoltPoint2D {
            sr_id: BoltInteger::new(7203),
            x: BoltFloat::new(1.0),
            y: BoltFloat::new(2.0),
        }),
    ];
    for value in values {
        let mut map = BoltMap::new();
        map.put(BoltString::new("native"), value.clone());
        for value in [value, BoltType::List(vec![BoltType::Map(map)].into())] {
            assert!(matches!(
                to_row(row(value)),
                Err(CypherQueryError::Result { .. })
            ));
        }
    }
}
