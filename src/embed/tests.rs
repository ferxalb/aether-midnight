//! Embedding tests. These use a *synthetic* deterministic embedder (a concept-hashing bag of words
//! with a tiny EN/ES synonym table) to exercise storage, isolation, fusion and lifecycle logic.
//! They say nothing about real model quality: that is covered by the CLI test that runs the real
//! multilingual-e5-small model when it is installed (`tests/cli_embeddings.rs`).

use super::*;
use std::sync::atomic::AtomicBool;
use tempfile::TempDir;

const CONCEPTS: &[&[&str]] = &[
    &["gato", "cat", "felino", "feline", "kitten"],
    &["sofa", "sofá", "couch", "sillon", "sillón"],
    &["factura", "bill", "invoice", "recibo"],
    &["electricidad", "electricity", "power", "luz"],
    &[
        "autenticacion",
        "autenticación",
        "authentication",
        "login",
        "auth",
    ],
];

struct HashEmbedder {
    identity: ModelIdentity,
}

impl HashEmbedder {
    fn new(revision: &str) -> Self {
        Self {
            identity: ModelIdentity {
                name: "test-hash".into(),
                revision: revision.into(),
                dimensions: 384,
            },
        }
    }
}

fn bucket(token: &str) -> usize {
    let hash = hash_bytes(token.as_bytes());
    usize::from_str_radix(&hash[..8], 16).unwrap() % 384
}

impl TextEmbedder for HashEmbedder {
    fn identity(&self) -> &ModelIdentity {
        &self.identity
    }
    fn embed(&mut self, _kind: TextKind, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0f32; 384];
                for word in text
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                {
                    let key = CONCEPTS
                        .iter()
                        .position(|group| group.contains(&word))
                        .map(|i| format!("concept{i}"))
                        .unwrap_or_else(|| word.to_string());
                    v[bucket(&key)] += 1.0;
                }
                l2_normalize(&mut v);
                v
            })
            .collect())
    }
}

fn never_loader() -> impl FnMut() -> Result<Box<dyn TextEmbedder>> {
    || panic!("the embedding model must not be loaded here")
}

fn hash_loader(revision: &'static str) -> impl FnMut() -> Result<Box<dyn TextEmbedder>> {
    move || Ok(Box::new(HashEmbedder::new(revision)) as Box<dyn TextEmbedder>)
}

fn db_in(temp: &TempDir) -> PathBuf {
    temp.path().join("m.sqlite3")
}

fn add(db: &Path, scope: &str, text: &str) -> i64 {
    memory::remember(
        db, scope, "source", text, None, None, None, None, None, None,
    )
    .unwrap()
    .id
}

fn vector_rows(db: &Path, kind: &str) -> i64 {
    open_db(db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM embeddings WHERE kind=?1",
            [kind],
            |r| r.get(0),
        )
        .unwrap()
}

fn no_cancel() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn e5_prefixes_and_unit_normalization() {
    assert_eq!(e5_input(TextKind::Query, "hola"), "query: hola");
    assert_eq!(e5_input(TextKind::Passage, "hola"), "passage: hola");
    let mut v = vec![3.0, 4.0];
    l2_normalize(&mut v);
    assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
    let mut zero = vec![0.0, 0.0];
    l2_normalize(&mut zero);
    assert_eq!(zero, vec![0.0, 0.0]);
    assert_eq!((MODEL_DIMS, MAX_INPUT_TOKENS), (384, 512));
}

#[test]
fn rrf_is_exact_and_breaks_ties_by_key() {
    let a = [1i64, 2, 3];
    let b = [3i64, 2, 9];
    let fused = rrf_fuse(&[&a, &b]);
    let expect = |ranks: &[usize]| ranks.iter().map(|r| 1.0 / (RRF_K + *r as f64)).sum::<f64>();
    // 3 (ranks 3 and 1) edges out 2 (ranks 2 and 2); then 1 (rank 1) before 9 (rank 3).
    assert_eq!(
        fused.iter().map(|x| x.0).collect::<Vec<_>>(),
        vec![3, 2, 1, 9]
    );
    assert!((fused[0].1 - expect(&[3, 1])).abs() < 1e-15);
    assert!((fused[1].1 - expect(&[2, 2])).abs() < 1e-15);
    // Equal scores (1 only in the first list at rank 1, 9 only in the second at rank 1) tie-break
    // by ascending key, independent of list order.
    let tie = rrf_fuse(&[&[7i64], &[5i64]]);
    assert_eq!(tie.iter().map(|x| x.0).collect::<Vec<_>>(), vec![5, 7]);
    assert_eq!(rrf_fuse(&[&[7i64], &[5i64]]), rrf_fuse(&[&[7i64], &[5i64]]));
}

#[test]
fn schema_v6_creates_embeddings_table_with_identity_columns() {
    let temp = TempDir::new().unwrap();
    let conn = open_db(&db_in(&temp)).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, 6);
    for column in [
        "model",
        "revision",
        "dims",
        "source_hash",
        "vector",
        "scope",
    ] {
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM pragma_table_info('embeddings') WHERE name=?1",
                [column],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "missing column {column}");
    }
}

#[test]
fn memory_vectors_persist_update_incrementally_and_follow_deletion() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let first = add(&db, "project:a", "El gato duerme en el sofá");
    let second = add(&db, "project:a", "monthly invoice for electricity");
    let mut embedder = HashEmbedder::new("r1");

    let report = embed_memories(&db, "project:a", &mut embedder, &no_cancel()).unwrap();
    assert_eq!((report.embedded, report.unchanged), (2, 0));
    assert_eq!(vector_rows(&db, "memory"), 2);
    let stored: (String, String, i64) = open_db(&db)
        .unwrap()
        .query_row(
            "SELECT model,revision,dims FROM embeddings WHERE owner_id=?1",
            [first],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, ("test-hash".into(), "r1".into(), 384));

    // Re-running is a no-op; a new memory is the only work.
    let again = embed_memories(&db, "project:a", &mut embedder, &no_cancel()).unwrap();
    assert_eq!((again.embedded, again.unchanged), (0, 2));
    add(&db, "project:a", "authentication uses login tokens");
    let third = embed_memories(&db, "project:a", &mut embedder, &no_cancel()).unwrap();
    assert_eq!((third.embedded, third.unchanged), (1, 2));

    // Forgetting removes the vector immediately (trigger), and search never returns it.
    assert!(memory::forget(&db, "project:a", second).unwrap());
    assert_eq!(vector_rows(&db, "memory"), 2);
    let hits = search_memory(
        &db,
        "project:a",
        "invoice electricity",
        SearchMode::Semantic,
        10,
        &mut hash_loader("r1"),
    )
    .unwrap();
    assert!(hits.results.iter().all(|r| r.memory.id != second));
    assert_eq!(
        embed_memories(&db, "project:a", &mut embedder, &no_cancel())
            .unwrap()
            .removed_stale,
        0
    );
}

#[test]
fn cross_language_semantic_and_hybrid_ranking_is_deterministic() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let cat_es = add(&db, "project:a", "El gato duerme en el sofá");
    add(&db, "project:a", "Quarterly roadmap and hiring plan");
    add(&db, "project:a", "authentication tokens rotate weekly");
    embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();

    // English query, Spanish document: no lexical overlap at all.
    let lexical = search_memory(
        &db,
        "project:a",
        "feline on the couch",
        SearchMode::Lexical,
        5,
        &mut never_loader(),
    )
    .unwrap();
    assert!(lexical.results.is_empty());
    let semantic = search_memory(
        &db,
        "project:a",
        "feline on the couch",
        SearchMode::Semantic,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    assert_eq!(semantic.results[0].memory.id, cat_es);
    assert!(!semantic.info.degraded);
    let hybrid = search_memory(
        &db,
        "project:a",
        "feline on the couch",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    assert_eq!(hybrid.info.mode_used, SearchMode::Hybrid);
    assert_eq!(hybrid.results[0].memory.id, cat_es);
    assert!(
        hybrid.results[0]
            .match_reason
            .starts_with("hybrid:semantic#1")
    );

    // Spanish query, lexical + semantic agree: fused score is the sum of both reciprocal ranks.
    let both = search_memory(
        &db,
        "project:a",
        "gato sofá",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    assert_eq!(both.results[0].memory.id, cat_es);
    assert!(
        both.results[0].match_reason.contains("lexical#1")
            && both.results[0].match_reason.contains("semantic#1")
    );
    assert!((both.results[0].score - 2.0 / (RRF_K + 1.0)).abs() < 1e-12);

    let ids = |r: &MemorySearchResponse| r.results.iter().map(|x| x.memory.id).collect::<Vec<_>>();
    let a = search_memory(
        &db,
        "project:a",
        "login weekly",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    let b = search_memory(
        &db,
        "project:a",
        "login weekly",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    assert_eq!(ids(&a), ids(&b));
}

#[test]
fn semantic_search_never_crosses_namespaces() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let mine = add(&db, "project:a", "un gato en el sofá");
    // The other namespace holds the *closest* possible match.
    let theirs = add(&db, "project:b", "cat couch feline sofa");
    for ns in ["project:a", "project:b"] {
        embed_memories(&db, ns, &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();
    }
    for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
        let r = search_memory(
            &db,
            "project:a",
            "cat couch feline sofa",
            mode,
            10,
            &mut hash_loader("r1"),
        )
        .unwrap();
        assert!(
            r.results
                .iter()
                .all(|x| x.memory.scope == "project:a" && x.memory.id != theirs)
        );
        assert_eq!(r.results[0].memory.id, mine);
    }
    // Embedding one namespace never creates vectors for another.
    let other = add(&db, "project:c", "gato");
    embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();
    let rows: i64 = open_db(&db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM embeddings WHERE owner_id=?1",
            [other],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn lexical_mode_and_missing_vectors_never_load_the_model() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    add(&db, "project:a", "alpha beta gamma");
    let lexical = search_memory(
        &db,
        "project:a",
        "alpha",
        SearchMode::Lexical,
        5,
        &mut never_loader(),
    )
    .unwrap();
    assert_eq!(lexical.results.len(), 1);
    assert!(!lexical.info.degraded);
    // No vectors stored: hybrid and semantic degrade to lexical *without* loading the model.
    for mode in [SearchMode::Hybrid, SearchMode::Semantic] {
        let r = search_memory(&db, "project:a", "alpha", mode, 5, &mut never_loader()).unwrap();
        assert!(r.info.degraded);
        assert_eq!(r.info.mode_used, SearchMode::Lexical);
        assert!(
            r.info
                .notice
                .as_deref()
                .unwrap()
                .contains("no embeddings stored")
        );
        assert_eq!(r.results.len(), 1);
    }
}

#[test]
fn loader_failure_degrades_with_reason_and_keeps_lexical_results() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    add(&db, "project:a", "alpha beta gamma");
    embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();
    let mut failing = || -> Result<Box<dyn TextEmbedder>> { bail!("model not installed") };
    let r = search_memory(
        &db,
        "project:a",
        "alpha",
        SearchMode::Hybrid,
        5,
        &mut failing,
    )
    .unwrap();
    assert!(r.info.degraded);
    assert!(
        r.info
            .notice
            .as_deref()
            .unwrap()
            .contains("model not installed")
    );
    assert_eq!(r.results.len(), 1);
}

#[test]
fn model_revision_change_replaces_vectors_and_old_vectors_are_not_used() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    add(&db, "project:a", "gato sofá");
    embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();
    // Querying with another revision must not compare against r1 vectors.
    let r = search_memory(
        &db,
        "project:a",
        "gato",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r2"),
    )
    .unwrap();
    assert!(r.info.degraded);
    assert!(r.info.notice.as_deref().unwrap().contains("do not match"));
    let report =
        embed_memories(&db, "project:a", &mut HashEmbedder::new("r2"), &no_cancel()).unwrap();
    assert_eq!((report.replaced_other_model, report.embedded), (1, 1));
    let revision: String = open_db(&db)
        .unwrap()
        .query_row("SELECT revision FROM embeddings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(revision, "r2");
}

#[test]
fn partial_coverage_is_reported() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    add(&db, "project:a", "gato sofá");
    embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &no_cancel()).unwrap();
    add(&db, "project:a", "factura luz");
    let r = search_memory(
        &db,
        "project:a",
        "gato",
        SearchMode::Hybrid,
        5,
        &mut hash_loader("r1"),
    )
    .unwrap();
    let coverage = r.info.semantic_coverage.unwrap();
    assert_eq!((coverage.embedded, coverage.eligible), (1, 2));
    assert!(r.info.notice.unwrap().contains("partial semantic coverage"));
}

#[test]
fn cancellation_stops_embedding_without_partial_rows() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    add(&db, "project:a", "gato");
    let cancel = AtomicBool::new(true);
    let report = embed_memories(&db, "project:a", &mut HashEmbedder::new("r1"), &cancel).unwrap();
    assert!(report.cancelled);
    assert_eq!(vector_rows(&db, "memory"), 0);
}

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn code_search(db: &Path, root: &Path, query: &str, mode: SearchMode) -> CodeHybridResponse {
    search_code(db, root, query, mode, 10, 4096, &mut hash_loader("r1")).unwrap()
}

#[test]
fn code_chunks_embed_search_reindex_and_delete() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let repo = temp.path().join("repo");
    write(
        &repo,
        "src/auth.rs",
        "// authentication login flow\nfn check_login() {}\n",
    );
    write(
        &repo,
        "src/billing.rs",
        "// factura electricidad\nfn compute_invoice() {}\n",
    );
    let cancel = no_cancel();
    code::index_repository(&db, &repo, false, false, &cancel).unwrap();
    let mut embedder = HashEmbedder::new("r1");
    let report = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert_eq!(report.embedded, 2);
    let again = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert_eq!((again.embedded, again.unchanged), (0, 2));

    // Spanish query, English content: only semantic can find it; source references are preserved.
    let semantic = code_search(&db, &repo, "autenticación", SearchMode::Semantic);
    assert_eq!(semantic.search.results[0].path, "src/auth.rs");
    assert_eq!(semantic.search.results[0].line_start, Some(1));
    assert!(
        semantic.search.results[0]
            .snippet
            .as_deref()
            .unwrap()
            .contains("check_login")
    );
    let hybrid = code_search(&db, &repo, "check_login autenticación", SearchMode::Hybrid);
    assert_eq!(hybrid.search.results[0].path, "src/auth.rs");
    assert!(hybrid.search.results[0].match_reason.starts_with("hybrid:"));

    // Edit + reindex: stale vectors vanish immediately (rehash trigger) and re-embed on demand.
    write(&repo, "src/auth.rs", "// factura recibo\nfn changed() {}\n");
    code::index_repository(&db, &repo, false, false, &cancel).unwrap();
    assert_eq!(vector_rows(&db, "code"), 1);
    let after = embed_code(&db, &repo, &mut embedder, &cancel).unwrap();
    assert_eq!((after.embedded, after.unchanged), (1, 1));

    // Deleting a file removes its vectors.
    std::fs::remove_file(repo.join("src/billing.rs")).unwrap();
    code::index_repository(&db, &repo, false, false, &cancel).unwrap();
    assert_eq!(vector_rows(&db, "code"), 1);
    let gone = code_search(&db, &repo, "factura electricidad", SearchMode::Semantic);
    assert!(
        gone.search
            .results
            .iter()
            .all(|r| r.path != "src/billing.rs")
    );
}

#[test]
fn code_semantic_hits_for_files_edited_after_indexing_are_dropped() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let repo = temp.path().join("repo");
    write(&repo, "a.rs", "// authentication login\nfn a() {}\n");
    let cancel = no_cancel();
    code::index_repository(&db, &repo, false, false, &cancel).unwrap();
    embed_code(&db, &repo, &mut HashEmbedder::new("r1"), &cancel).unwrap();
    write(&repo, "a.rs", "// something else entirely\n");
    let r = code_search(&db, &repo, "authentication", SearchMode::Semantic);
    assert!(r.search.results.is_empty());
}

#[test]
fn code_vectors_are_isolated_between_repositories() {
    let temp = TempDir::new().unwrap();
    let db = db_in(&temp);
    let one = temp.path().join("one");
    let two = temp.path().join("two");
    write(&one, "x.rs", "// gato sofá\n");
    write(&two, "y.rs", "// cat couch feline sofa\n");
    let cancel = no_cancel();
    for repo in [&one, &two] {
        code::index_repository(&db, repo, false, false, &cancel).unwrap();
        embed_code(&db, repo, &mut HashEmbedder::new("r1"), &cancel).unwrap();
    }
    let r = code_search(&db, &one, "cat couch feline sofa", SearchMode::Semantic);
    assert!(r.search.results.iter().all(|x| x.path == "x.rs"));
    assert_eq!(r.search.results.len(), 1);
}

#[test]
fn chunking_splits_long_files_and_preserves_line_ranges() {
    let text: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let (chunks, truncated) = chunk_text(&text);
    assert!(!truncated);
    assert_eq!(chunks[0].line_start, 1);
    assert_eq!(chunks.last().unwrap().line_end, 100);
    for pair in chunks.windows(2) {
        assert_eq!(pair[1].line_start, pair[0].line_end + 1);
    }
    assert!(chunks.iter().all(|c| c.text.len() <= CHUNK_MAX_BYTES + 16));
    assert!(chunk_text("\n\n  \n").0.is_empty());
}
