//! Editing one part of a file must re-embed only the chunks whose text changed.

use std::fs;
use std::sync::atomic::AtomicBool;

use aether_midnight::code::index_repository;
use aether_midnight::embed::{ModelIdentity, TextEmbedder, TextKind, embed_code};
use tempfile::TempDir;

const DIMS: usize = 4;

/// Deterministic embedder that records how many texts it was asked to embed.
struct Counting {
    identity: ModelIdentity,
    texts_seen: usize,
}

impl TextEmbedder for Counting {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }

    fn embed(&mut self, _kind: TextKind, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.texts_seen += texts.len();
        Ok(texts.iter().map(|_| vec![0.5, 0.5, 0.5, 0.5]).collect())
    }
}

fn source(lines: usize, marker: &str) -> String {
    let mut text = String::new();
    for i in 0..lines {
        text.push_str(&format!("fn f{i}() -> u32 {{ {i} }} // {marker}\n"));
    }
    text
}

#[test]
fn editing_one_chunk_reembeds_only_that_chunk() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let db = temp.path().join("db.sqlite3");
    let cancel = AtomicBool::new(false);
    let mut embedder = Counting {
        identity: ModelIdentity {
            name: "test-model".into(),
            revision: "r1".into(),
            dimensions: DIMS,
        },
        texts_seen: 0,
    };

    // 200 lines are several 40-line chunks.
    let original = source(200, "a");
    fs::write(repo.join("lib.rs"), &original).unwrap();
    index_repository(&db, &repo, false, false, &cancel).unwrap();
    let first = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert!(first.embedded >= 4, "expected several chunks: {first:?}");
    let total = first.embedded;
    assert_eq!(embedder.texts_seen, total);

    // Change one line in the last chunk only; line counts stay identical.
    let mut lines: Vec<String> = original.lines().map(String::from).collect();
    lines[199] = "fn changed() -> u32 { 999 }".into();
    fs::write(repo.join("lib.rs"), lines.join("\n") + "\n").unwrap();
    index_repository(&db, &repo, false, false, &cancel).unwrap();
    embedder.texts_seen = 0;
    let second = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert_eq!(second.embedded, 1, "only the edited chunk: {second:?}");
    assert_eq!(second.unchanged, total - 1);
    assert_eq!(embedder.texts_seen, 1, "unchanged chunks must not be sent");

    // A third run with no edits embeds nothing.
    embedder.texts_seen = 0;
    let third = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert_eq!(third.embedded, 0);
    assert_eq!(embedder.texts_seen, 0);
}
