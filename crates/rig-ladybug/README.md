# rig-ladybug

Native embedded Ladybug Cypher queries implementing Rig's `CypherQuery` contract.
The client is cloneable and owns one database handle. Queries create their own
connections on Tokio's blocking pool and return owned rows.

```rust,no_run
use rig_core::cypher::{CypherQuery, CypherRequest};
use rig_ladybug::LadybugClient;
use serde_json::json;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let client = LadybugClient::open("conversation.graph")?;
let rows = client.query(
    CypherRequest::new("RETURN $name AS name").param("name", json!("Alice")),
).await?;
# let _ = rows;
# Ok(()) }
```

Project scalar values, homogeneous lists, string-keyed maps, or structs under
unique column aliases. Integers must fit signed 64 bits and floats must be finite.
Nulls and nested supported values are preserved. Heterogeneous list parameters,
native nodes, relationships, paths, temporal values, blobs, decimals, UUIDs, and
other unsupported native values produce conversion errors. List parameters use
Ladybug's typed lists; empty and all-null lists default to the integer element type.

`transaction(Vec<CypherRequest>)` executes statements on one connection, commits
only after all result conversions succeed, and rolls back on failure. Requests
must not include transaction-control statements. A dropped async future does not
cancel native work that has already started. Statements can modify data; apply
application access controls before accepting caller-provided queries.

The crate pins `lbug` to `0.20.4` and disables Arrow. The SDK's default build can
download the latest native library independently of its Rust version. Set
`LBUG_VERSION=0.20.4` to select the matching precompiled release, or set
`LBUG_BUILD_FROM_SOURCE=1` to build the bundled pinned C++ source. Source builds
require a C++ compiler, CMake, and OpenSSL development libraries. This integration
is native-only and requires a Tokio runtime for async operations.
