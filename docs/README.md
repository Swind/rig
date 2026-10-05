# Architecture documentation

These documents describe implemented APIs and their responsibilities:

- [Conversation search](architecture/conversation-search.md): the search
  contract, original-message references, portable tool, and backend boundary.
- [Cypher queries](architecture/cypher.md): the shared execution contract and
  Neo4j parameter/result conversion.

## Historical proposals

- [Generic retrieval and GraphRAG](proposals/generic-retrieval-graphrag.md)
  preserves an earlier, broader proposal. Its proposed retrieval and graph
  abstractions are not implemented and do not describe the current architecture.
