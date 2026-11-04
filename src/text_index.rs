//! Narrow internal boundary for deterministic repository-text retrieval.
//! SQLite FTS5 unicode61 is the v1 backend.

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
    fn insert(&self, transaction: &Transaction<'_>, document_id: i64, content: &str) -> Result<()>;
    fn replace(&self, transaction: &Transaction<'_>, document_id: i64, content: &str)
    -> Result<()>;
    fn remove_stale(
        &self,
        transaction: &Transaction<'_>,
        repo_id: &str,
        worktree_id: &str,
        generation: u64,
    ) -> Result<()>;
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
pub(crate) struct SqliteFtsIndex;

impl TextIndex for SqliteFtsIndex {
    fn insert(&self, transaction: &Transaction<'_>, document_id: i64, content: &str) -> Result<()> {
        transaction
            .prepare_cached("INSERT INTO code_fts(rowid,content) VALUES(?1,?2)")?
            .execute(params![document_id, content])?;
        Ok(())
    }

    fn replace(
        &self,
        transaction: &Transaction<'_>,
        document_id: i64,
        content: &str,
    ) -> Result<()> {
        transaction
            .prepare_cached("DELETE FROM code_fts WHERE rowid=?1")?
            .execute([document_id])?;
        self.insert(transaction, document_id, content)?;
        Ok(())
    }

    fn remove_stale(
        &self,
        transaction: &Transaction<'_>,
        repo_id: &str,
        worktree_id: &str,
        generation: u64,
    ) -> Result<()> {
        transaction.prepare_cached(
            "DELETE FROM code_fts WHERE rowid IN (
                 SELECT id FROM code_files WHERE repo_id=?1 AND worktree_id=?2 AND indexed_generation<>?3
             )",
        )?.execute(params![repo_id, worktree_id, generation as i64])?;
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
            "SELECT f.id,f.rel_path,f.content_hash,f.indexed_generation,bm25(code_fts)
             FROM code_fts JOIN code_files f ON f.id=code_fts.rowid
             WHERE code_fts MATCH ?1 AND f.repo_id=?2 AND f.worktree_id=?3
             ORDER BY bm25(code_fts),f.rel_path LIMIT ?4",
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
