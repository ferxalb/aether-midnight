//! Narrow internal boundary for deterministic repository-text retrieval.
//! The SQLite FTS5 trigram implementation is the v1 backend.

use anyhow::Result;
use rusqlite::{Connection, Transaction, params};

#[derive(Debug)]
pub(crate) struct TextHit {
    pub document_id: i64,
    pub path: String,
    pub content_hash: String,
    pub generation: u64,
    pub rank: f64,
}

pub(crate) trait TextIndex {
    fn replace(&self, transaction: &Transaction<'_>, document_id: i64, content: &str)
    -> Result<()>;
    fn remove(&self, transaction: &Transaction<'_>, document_id: i64) -> Result<()>;
    fn search(
        &self,
        connection: &Connection,
        repo_id: &str,
        worktree_id: &str,
        match_query: &str,
        limit: usize,
    ) -> Result<Vec<TextHit>>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SqliteTrigramIndex;

impl TextIndex for SqliteTrigramIndex {
    fn replace(
        &self,
        transaction: &Transaction<'_>,
        document_id: i64,
        content: &str,
    ) -> Result<()> {
        transaction.execute("DELETE FROM code_trigram WHERE rowid=?1", [document_id])?;
        transaction.execute(
            "INSERT INTO code_trigram(rowid,content) VALUES(?1,?2)",
            params![document_id, content],
        )?;
        Ok(())
    }

    fn remove(&self, transaction: &Transaction<'_>, document_id: i64) -> Result<()> {
        transaction.execute("DELETE FROM code_trigram WHERE rowid=?1", [document_id])?;
        Ok(())
    }

    fn search(
        &self,
        connection: &Connection,
        repo_id: &str,
        worktree_id: &str,
        match_query: &str,
        limit: usize,
    ) -> Result<Vec<TextHit>> {
        let mut statement = connection.prepare(
            "SELECT f.id,f.rel_path,f.content_hash,f.indexed_generation,bm25(code_trigram)
             FROM code_trigram JOIN code_files f ON f.id=code_trigram.rowid
             WHERE code_trigram MATCH ?1 AND f.repo_id=?2 AND f.worktree_id=?3
             ORDER BY bm25(code_trigram),f.rel_path LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![match_query, repo_id, worktree_id, limit as i64],
            |row| {
                Ok(TextHit {
                    document_id: row.get(0)?,
                    path: row.get(1)?,
                    content_hash: row.get(2)?,
                    generation: row.get::<_, i64>(3)? as u64,
                    rank: row.get(4)?,
                })
            },
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
