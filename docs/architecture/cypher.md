# Cypher query architecture

Rig's `CypherQuery` contract executes parameterized statements and returns
named JSON projections. `rig-neo4j` implements it for `Neo4jClient` alongside
its vector-search support. Cypher execution requires no embedding model or
vector index.

## Components and public paths

| Component | Public path | Implementation |
| --- | --- | --- |
| Execution contract and data types | `rig::cypher` or `rig_core::cypher` | [cypher.rs](../../crates/rig-core/src/cypher.rs) |
| Neo4j implementation | `rig::neo4j::Neo4jClient` with facade feature `neo4j`, or `rig_neo4j::Neo4jClient` | [Neo4j adapter](../../crates/rig-neo4j/src/cypher.rs) |

`CypherRequest` contains a statement and a `BTreeMap<String, serde_json::Value>`
of parameters. `new(statement)` starts with no parameters; `param(name, value)`
binds a value and replaces an existing binding of that name. Parameter names
omit the `$` prefix. Values are bound separately from the statement.

`CypherQuery::query` returns `Vec<CypherRow>`, where each row is a
`BTreeMap<String, Value>` keyed by unique column aliases. It preserves backend
row order, returns an empty vector for no rows, and materializes all returned
rows. Callers use `ORDER BY` and statement-level limits when needed.

The contract uses Rig's WASM-compatible future and trait bounds. Its boxed
error sources require `Send + Sync` only on native targets. This portability
of the core contract does not establish WASM support for the Neo4j driver.

## Neo4j execution and conversion

The adapter converts every parameter before executing the statement. It binds
the converted values with `neo4rs`, executes through the client's graph
connection, consumes the row stream, and converts named columns to JSON.

Supported values are null, booleans, strings, signed 64-bit integers, finite
floating-point numbers, and nested lists and maps. JSON unsigned integers
above `i64::MAX` are rejected before database execution. Conversion rejects
unsupported values recursively, including native nodes, relationships, paths,
temporal values, spatial values, and bytes. Queries must project those values
to supported JSON representations.

Strict row deserialization preserves column aliases, including a single map
column. A custom projection deserializer checks Bolt type tags before
converting values. This prevents driver conversions such as treating a native
Duration as a JSON list. Non-finite result floats also produce errors.

## Errors and execution semantics

| Error variant | Failure phase |
| --- | --- |
| `Parameter { name, source }` | A named parameter cannot be represented by the backend |
| `Query { source }` | Statement execution or row streaming fails |
| `Result { source }` | A returned value cannot be represented as a JSON projection |

Each error retains its original source. Result-conversion errors do not imply
that database writes were rolled back. Transaction behavior follows the backend.

Statements use the backend's Cypher dialect and connection permissions, and
may modify data. The interface performs no read-only enforcement or automatic
effect-bus recording. It is an execution primitive; the
[conversation search backend](conversation-search.md#backend-boundary) may
use it to implement application-specific graph queries. Rig supplies no generic
graph traversal model or Cypher agent tool.

## Usage and regression coverage

The [Neo4j usage guide](../../crates/rig-neo4j/README.md#cypher-queries) shows
connection setup, parameter binding, and named projections.
[Core tests](../../crates/rig-core/src/cypher/tests.rs) exercise a mock consumer
and error sources. [Adapter tests](../../crates/rig-neo4j/src/cypher/tests.rs)
exercise strict JSON conversion. The
[Neo4j integration target](../../tests/integrations/neo4j.rs) runs the Cypher
contract and existing vector search against the pinned `neo4j:5.26.29` Docker
image when Docker is available.
