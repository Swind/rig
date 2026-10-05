use std::path::Path;

use rig_core::{
    completion::Message, conversation_search::ConversationSearchHit, id::ConversationId,
};
use rusqlite::{OptionalExtension, Transaction, params};
use tokio_rusqlite::Connection;

use crate::chunking::{complete_exchange, embedding_text};

#[derive(Debug, thiserror::Error)]
pub(crate) enum StorageError {
    #[error("SQLite conversation storage failed: {0}")]
    Sqlite(#[from] tokio_rusqlite::Error),
    #[error("SQLite statement failed: {0}")]
    Statement(#[from] rusqlite::Error),
    #[error("Message serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Stored indexing configuration differs; rebuild with a separate database")]
    IncompatibleConfiguration,
    #[error("Unsupported conversation storage schema version: {0}")]
    UnsupportedSchema(i64),
    #[error("Conversation storage integer is out of range")]
    IntegerRange,
}

#[derive(Clone)]
pub(crate) struct Storage {
    connection: Connection,
}

#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    pub id: String,
    pub scope: String,
    pub conversation_id: ConversationId,
    pub generation: u64,
    pub start: u64,
    pub end: u64,
    pub text: String,
    pub retired: bool,
    pub vector_done: bool,
}

const CHUNK_COLUMNS: &str =
    "id, scope, conversation, generation, start, end, text, retired, vector_done";

fn chunk_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chunk> {
    Ok(Chunk {
        id: row.get(0)?,
        scope: row.get(1)?,
        conversation_id: ConversationId::new(row.get::<_, String>(2)?),
        generation: row.get(3)?,
        start: row.get(4)?,
        end: row.get(5)?,
        text: row.get(6)?,
        retired: row.get(7)?,
        vector_done: row.get(8)?,
    })
}

fn read_messages(
    transaction: &Transaction<'_>,
    scope: &str,
    conversation: &str,
    generation: i64,
    start: i64,
    end: i64,
) -> Result<Vec<Message>, StorageError> {
    let mut statement = transaction.prepare(
        "SELECT message FROM messages WHERE scope=?1 AND conversation=?2 AND generation=?3 AND position>=?4 AND position<?5 ORDER BY position",
    )?;
    let values = statement.query_map(
        params![scope, conversation, generation, start, end],
        |row| row.get::<_, String>(0),
    )?;
    values
        .map(|value| Ok(serde_json::from_str(&value?)?))
        .collect()
}

fn active_chunk(
    transaction: &Transaction<'_>,
    scope: &str,
    id: &str,
) -> Result<Option<Chunk>, StorageError> {
    Ok(transaction
        .query_row(
            &format!(
                "SELECT {CHUNK_COLUMNS} FROM chunks WHERE id=?1 AND scope=?2 AND retired=0 AND generation=(SELECT generation FROM conversations WHERE conversations.scope=chunks.scope AND conversations.conversation=chunks.conversation)"
            ),
            params![id, scope],
            chunk_from_row,
        )
        .optional()?)
}

impl Storage {
    pub async fn open(
        path: impl AsRef<Path>,
        index_identity: String,
    ) -> Result<Self, StorageError> {
        let connection = Connection::open(path).await?;
        connection.call(move |connection| {
            let result = (|| -> Result<(), StorageError> {
                connection.busy_timeout(std::time::Duration::from_secs(5))?;
                connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
                let transaction = connection.transaction()?;
                let version: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;
                if version != 0 && version != 1 {
                    return Err(StorageError::UnsupportedSchema(version));
                }
                transaction.execute_batch(
                    "CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                     CREATE TABLE IF NOT EXISTS conversations (
                       scope TEXT NOT NULL, conversation TEXT NOT NULL,
                       generation INTEGER NOT NULL DEFAULT 0, next_position INTEGER NOT NULL DEFAULT 0,
                       indexed_until INTEGER NOT NULL DEFAULT 0,
                       PRIMARY KEY(scope,conversation));
                     CREATE TABLE IF NOT EXISTS messages (
                       scope TEXT NOT NULL, conversation TEXT NOT NULL, generation INTEGER NOT NULL,
                       position INTEGER NOT NULL, message TEXT NOT NULL,
                       PRIMARY KEY(scope,conversation,generation,position));
                     CREATE TABLE IF NOT EXISTS chunks (
                       id TEXT PRIMARY KEY, scope TEXT NOT NULL, conversation TEXT NOT NULL,
                       generation INTEGER NOT NULL, start INTEGER NOT NULL, end INTEGER NOT NULL,
                       text TEXT NOT NULL, chunking_version INTEGER NOT NULL DEFAULT 1,
                       retired INTEGER NOT NULL DEFAULT 0, vector_done INTEGER NOT NULL DEFAULT 0,
                       attempts INTEGER NOT NULL DEFAULT 0,
                       last_error TEXT);
                     CREATE INDEX IF NOT EXISTS chunks_pending ON chunks(scope,retired,vector_done);
                     CREATE INDEX IF NOT EXISTS chunks_start ON chunks(scope,conversation,generation,start);
                     CREATE INDEX IF NOT EXISTS chunks_end ON chunks(scope,conversation,generation,end);
                     PRAGMA user_version=1;",
                )?;
                let stored = transaction.query_row("SELECT value FROM metadata WHERE key='index_identity'", [], |row| row.get::<_,String>(0)).optional()?;
                match stored {
                    Some(stored) if stored != index_identity => return Err(StorageError::IncompatibleConfiguration),
                    None => { transaction.execute("INSERT INTO metadata(key,value) VALUES('index_identity',?1)", [index_identity])?; }
                    Some(_) => {}
                }
                transaction.commit()?;
                Ok(())
            })();
            Ok(result)
        }).await??;
        Ok(Self { connection })
    }

    pub async fn append(
        &self,
        scope: &str,
        conversation_id: &ConversationId,
        messages: Vec<Message>,
        text_budget: usize,
    ) -> Result<(), StorageError> {
        if messages.is_empty() {
            return Ok(());
        }
        let scope = scope.to_owned();
        let conversation = conversation_id.to_string();
        self.connection.call(move |connection| {
            let result = (|| -> Result<(), StorageError> {
                let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                transaction.execute("INSERT OR IGNORE INTO conversations(scope,conversation) VALUES(?1,?2)", params![scope,conversation])?;
                let (generation,start,indexed_until): (i64,i64,i64) = transaction.query_row("SELECT generation,next_position,indexed_until FROM conversations WHERE scope=?1 AND conversation=?2", params![scope,conversation], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                let count = i64::try_from(messages.len()).map_err(|_| StorageError::IntegerRange)?;
                let end = start.checked_add(count).ok_or(StorageError::IntegerRange)?;
                for (offset, message) in messages.iter().enumerate() {
                    let position = start + i64::try_from(offset).map_err(|_| StorageError::IntegerRange)?;
                    transaction.execute("INSERT INTO messages(scope,conversation,generation,position,message) VALUES(?1,?2,?3,?4,?5)", params![scope,conversation,generation,position,serde_json::to_string(message)?])?;
                }
                let tail = read_messages(&transaction, &scope, &conversation, generation, indexed_until, end)?;
                let completed = complete_exchange(&tail);
                if completed {
                    transaction.execute("INSERT INTO chunks(id,scope,conversation,generation,start,end,text) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![uuid::Uuid::new_v4().to_string(),scope,conversation,generation,indexed_until,end,embedding_text(&tail,text_budget)])?;
                }
                transaction.execute("UPDATE conversations SET next_position=?3,indexed_until=?4 WHERE scope=?1 AND conversation=?2", params![scope,conversation,end,if completed {end} else {indexed_until}])?;
                transaction.commit()?;
                Ok(())
            })();
            Ok(result)
        }).await?
    }

    pub async fn load(
        &self,
        scope: &str,
        conversation_id: &ConversationId,
    ) -> Result<Vec<Message>, StorageError> {
        let scope = scope.to_owned();
        let conversation = conversation_id.to_string();
        self.connection.call(move |connection| {
            let result = (|| -> Result<Vec<Message>, StorageError> {
                let transaction = connection.transaction()?;
                let current: Option<(i64,i64)> = transaction.query_row("SELECT generation,next_position FROM conversations WHERE scope=?1 AND conversation=?2", params![scope,conversation], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
                let messages = match current {
                    Some((generation,end)) => read_messages(&transaction,&scope,&conversation,generation,0,end)?,
                    None => Vec::new(),
                };
                transaction.commit()?;
                Ok(messages)
            })();
            Ok(result)
        }).await?
    }

    pub async fn clear(
        &self,
        scope: &str,
        conversation_id: &ConversationId,
    ) -> Result<(), StorageError> {
        let scope = scope.to_owned();
        let conversation = conversation_id.to_string();
        self.connection.call(move |connection| {
            let result = (|| -> Result<(), StorageError> {
                let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                transaction.execute("INSERT OR IGNORE INTO conversations(scope,conversation) VALUES(?1,?2)", params![scope,conversation])?;
                let generation: i64 = transaction.query_row("SELECT generation FROM conversations WHERE scope=?1 AND conversation=?2", params![scope,conversation], |row| row.get(0))?;
                let next = generation.checked_add(1).ok_or(StorageError::IntegerRange)?;
                transaction.execute("UPDATE conversations SET generation=?3,next_position=0,indexed_until=0 WHERE scope=?1 AND conversation=?2", params![scope,conversation,next])?;
                transaction.execute("DELETE FROM messages WHERE scope=?1 AND conversation=?2", params![scope,conversation])?;
                transaction.execute("UPDATE chunks SET retired=1,text='',vector_done=0 WHERE scope=?1 AND conversation=?2 AND retired=0", params![scope,conversation])?;
                transaction.commit()?;
                Ok(())
            })();
            Ok(result)
        }).await?
    }

    pub async fn pending(&self, scope: &str, limit: usize) -> Result<Vec<Chunk>, StorageError> {
        let scope = scope.to_owned();
        let limit = i64::try_from(limit).map_err(|_| StorageError::IntegerRange)?;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare(&format!("SELECT {CHUNK_COLUMNS} FROM chunks WHERE scope=?1 AND vector_done=0 ORDER BY retired DESC,attempts,conversation,generation,start,id LIMIT ?2"))?;
            Ok(statement.query_map(params![scope,limit],chunk_from_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.map_err(StorageError::from)
    }

    pub async fn mark_projection(
        &self,
        id: &str,
        expected_retired: bool,
    ) -> Result<(), StorageError> {
        let id = id.to_owned();
        self.connection.call(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute("UPDATE chunks SET vector_done=1,last_error=NULL WHERE id=?1 AND retired=?2 AND (retired=1 OR generation=(SELECT generation FROM conversations WHERE conversations.scope=chunks.scope AND conversations.conversation=chunks.conversation))", params![id,expected_retired])?;
            transaction.execute("DELETE FROM chunks WHERE id=?1 AND retired=1 AND vector_done=1", [&id])?;
            transaction.commit()?;
            Ok(())
        }).await.map_err(StorageError::from)
    }

    pub async fn record_failure(&self, id: &str, error: &str) -> Result<(), StorageError> {
        let id = id.to_owned();
        let error = error.to_owned();
        self.connection
            .call(move |connection| {
                connection.execute(
                    "UPDATE chunks SET attempts=attempts+1,last_error=?2 WHERE id=?1",
                    params![id, error],
                )?;
                Ok(())
            })
            .await
            .map_err(StorageError::from)
    }

    pub async fn rebuild(&self, scope: &str) -> Result<(), StorageError> {
        let scope = scope.to_owned();
        self.connection
            .call(move |connection| {
                connection.execute(
                    "UPDATE chunks SET vector_done=0,last_error=NULL WHERE scope=?1 AND retired=0 AND generation=(SELECT generation FROM conversations WHERE conversations.scope=chunks.scope AND conversations.conversation=chunks.conversation)",
                    [scope],
                )?;
                Ok(())
            })
            .await
            .map_err(StorageError::from)
    }

    pub async fn status(&self, scope: &str) -> Result<crate::IndexStatus, StorageError> {
        let scope = scope.to_owned();
        self.connection.call(move |connection| {
            let (pending_jobs,pending_vector,failed_attempts) = connection.query_row("SELECT COUNT(*),COALESCE(SUM(vector_done=0),0),COALESCE(SUM(attempts),0) FROM chunks WHERE scope=?1 AND vector_done=0",[&scope], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
            let last_error = connection.query_row("SELECT last_error FROM chunks WHERE scope=?1 AND last_error IS NOT NULL ORDER BY rowid DESC LIMIT 1",[scope], |row| row.get::<_,String>(0)).optional()?;
            Ok(crate::IndexStatus {pending_jobs,pending_vector,failed_attempts,last_error})
        }).await.map_err(StorageError::from)
    }

    pub async fn active(&self, scope: &str, ids: &[String]) -> Result<Vec<Chunk>, StorageError> {
        let scope = scope.to_owned();
        let ids = ids.to_owned();
        self.connection
            .call(move |connection| {
                let result = (|| -> Result<Vec<Chunk>, StorageError> {
                    let transaction = connection.transaction()?;
                    let mut chunks = Vec::new();
                    for id in ids {
                        if let Some(chunk) = active_chunk(&transaction, &scope, &id)? {
                            chunks.push(chunk);
                        }
                    }
                    transaction.commit()?;
                    Ok(chunks)
                })();
                Ok(result)
            })
            .await?
    }

    pub async fn neighbors(&self, seed: &Chunk, limit: usize) -> Result<Vec<String>, StorageError> {
        let seed = seed.clone();
        let limit = i64::try_from(limit).map_err(|_| StorageError::IntegerRange)?;
        self.connection.call(move |connection| {
            let result = (|| -> Result<Vec<String>, StorageError> {
                let transaction = connection.transaction()?;
                let Some(current) = active_chunk(&transaction, &seed.scope, &seed.id)? else {
                    return Ok(Vec::new());
                };
                if current.generation != seed.generation || current.conversation_id != seed.conversation_id {
                    return Ok(Vec::new());
                }
                let generation = i64::try_from(current.generation).map_err(|_| StorageError::IntegerRange)?;
                let start = i64::try_from(current.start).map_err(|_| StorageError::IntegerRange)?;
                let end = i64::try_from(current.end).map_err(|_| StorageError::IntegerRange)?;
                let ids = {
                    let mut statement = transaction.prepare(
                        "SELECT id FROM chunks WHERE scope=?1 AND conversation=?2 AND generation=?3 AND retired=0 AND id<>?4 AND (end=?5 OR start=?6) ORDER BY start,id LIMIT ?7",
                    )?;
                    statement.query_map(params![current.scope,current.conversation_id.as_str(),generation,current.id,start,end,limit], |row| row.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
                };
                transaction.commit()?;
                Ok(ids)
            })();
            Ok(result)
        }).await?
    }

    pub async fn hydrate(
        &self,
        scope: &str,
        ids: &[String],
        conversation_id: Option<&ConversationId>,
        max_bytes: usize,
    ) -> Result<Vec<ConversationSearchHit>, StorageError> {
        let scope = scope.to_owned();
        let ids = ids.to_owned();
        let filter = conversation_id.cloned();
        self.connection
            .call(move |connection| {
                let result = (|| -> Result<Vec<ConversationSearchHit>, StorageError> {
                    let transaction = connection.transaction()?;
                    let mut hits = Vec::new();
                    let mut budget = max_bytes.saturating_sub(2);
                    let mut seen = std::collections::HashSet::new();
                    for id in ids {
                        if !seen.insert(id.clone()) {
                            continue;
                        }
                        let Some(chunk) = active_chunk(&transaction, &scope, &id)? else {
                            continue;
                        };
                        if filter
                            .as_ref()
                            .is_some_and(|filter| filter != &chunk.conversation_id)
                        {
                            continue;
                        }
                        let generation = i64::try_from(chunk.generation)
                            .map_err(|_| StorageError::IntegerRange)?;
                        let start =
                            i64::try_from(chunk.start).map_err(|_| StorageError::IntegerRange)?;
                        let end =
                            i64::try_from(chunk.end).map_err(|_| StorageError::IntegerRange)?;
                        let messages = read_messages(
                            &transaction,
                            &scope,
                            chunk.conversation_id.as_str(),
                            generation,
                            start,
                            end,
                        )?;
                        let hit = ConversationSearchHit {
                            conversation_id: chunk.conversation_id,
                            chunk_id: chunk.id,
                            start_index: chunk.start,
                            messages,
                        };
                        let bytes = serde_json::to_vec(&hit)?.len() + usize::from(!hits.is_empty());
                        if bytes > budget {
                            continue;
                        }
                        budget -= bytes;
                        hits.push(hit);
                    }
                    transaction.commit()?;
                    Ok(hits)
                })();
                Ok(result)
            })
            .await?
    }
}

#[cfg(test)]
mod tests;
