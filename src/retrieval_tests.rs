//! Exhaustive-oracle and adversarial regression tests for memory retrieval.
//!
//! The oracle scores *every* eligible memory with a from-scratch BM25 implementation
//! (validated against SQLite FTS5's own `rank` below) and applies the documented ordering:
//! exact phrase, distinct informative terms matched, BM25, then deterministic tie-breaks.
//! Agreement with the oracle proves exact top-k under that scoring function. It is not a claim
//! of semantic relevance; labeled-judgment tests cover that separately.

use std::collections::HashMap;

use rusqlite::Connection;
use tempfile::TempDir;

use crate::db::normalize_timestamp;
use crate::memory::{
    self, CandidateCoverage, MemoryInput, RecallCandidateOrder, contains_contiguous_terms,
    informative_terms, normalized_tokens, token_vec,
};

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;
const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

#[derive(Clone, Debug)]
struct Doc {
    id: i64,
    scope: String,
    kind: String,
    content: String,
    source_uri: Option<String>,
    source_id: Option<i64>,
    supersedes_id: Option<i64>,
    observed_at: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
}

#[derive(Debug, Clone)]
struct OracleHit {
    id: i64,
    exact_phrase: bool,
    matched_terms: usize,
    score: f64,
}

struct Oracle {
    docs: Vec<Doc>,
    tokens: Vec<Vec<String>>,
    avg_len: f64,
    hits_cache: std::cell::RefCell<HashMap<Vec<String>, f64>>,
}

fn load_corpus(path: &std::path::Path) -> Vec<Doc> {
    let connection = Connection::open(path).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT id,scope,kind,content,source_uri,source_id,supersedes_id,observed_at,valid_from,valid_to
             FROM memories WHERE deleted_at IS NULL ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(Doc {
                id: row.get(0)?,
                scope: row.get(1)?,
                kind: row.get(2)?,
                content: row.get(3)?,
                source_uri: row.get(4)?,
                source_id: row.get(5)?,
                supersedes_id: row.get(6)?,
                observed_at: row.get(7)?,
                valid_from: row.get(8)?,
                valid_to: row.get(9)?,
            })
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn phrase_count(tokens: &[String], phrase: &[String]) -> usize {
    if phrase.is_empty() || phrase.len() > tokens.len() {
        return 0;
    }
    tokens
        .windows(phrase.len())
        .filter(|window| window.iter().eq(phrase.iter()))
        .count()
}

impl Oracle {
    fn new(docs: Vec<Doc>) -> Self {
        let tokens = docs
            .iter()
            .map(|doc| token_vec(&doc.content))
            .collect::<Vec<_>>();
        // FTS5 counts the one-token scope column in every document length.
        let total: usize = tokens.iter().map(|tokens| tokens.len() + 1).sum();
        let avg_len = total as f64 / docs.len().max(1) as f64;
        Self {
            docs,
            tokens,
            avg_len,
            hits_cache: Default::default(),
        }
    }

    /// FTS5 `bm25()` over `phrases`, with weight 1 for `content` and 0 for `scope_token`.
    fn bm25(&self, index: usize, phrases: &[Vec<String>]) -> f64 {
        let rows = self.docs.len() as f64;
        let doc_len = (self.tokens[index].len() + 1) as f64;
        phrases
            .iter()
            .map(|phrase| {
                let hits = *self
                    .hits_cache
                    .borrow_mut()
                    .entry(phrase.clone())
                    .or_insert_with(|| {
                        self.tokens
                            .iter()
                            .filter(|tokens| phrase_count(tokens, phrase) > 0)
                            .count() as f64
                    });
                let mut idf = ((rows - hits + 0.5) / (hits + 0.5)).ln();
                if idf <= 0.0 {
                    idf = 1e-6;
                }
                let tf = phrase_count(&self.tokens[index], phrase) as f64;
                idf * (tf * (BM25_K1 + 1.0))
                    / (tf + BM25_K1 * (1.0 - BM25_B + BM25_B * doc_len / self.avg_len))
            })
            .sum()
    }

    fn rank(&self, scope: &str, query: &str, at: &str, limit: usize) -> Vec<OracleHit> {
        let at = normalize_timestamp(at).unwrap();
        let full_terms = normalized_tokens(query);
        let terms = informative_terms(&full_terms);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut phrases = Vec::new();
        if full_terms.len() > 1 {
            phrases.push(full_terms.clone());
        }
        phrases.extend(terms.iter().map(|term| vec![term.clone()]));
        let required = 2.min(terms.len());
        let mut hits = Vec::new();
        for (index, doc) in self.docs.iter().enumerate() {
            if doc.scope != scope
                || doc
                    .valid_from
                    .as_deref()
                    .is_some_and(|from| from > at.as_str())
                || doc.valid_to.as_deref().is_some_and(|to| to <= at.as_str())
            {
                continue;
            }
            let matched_terms = terms
                .iter()
                .filter(|term| self.tokens[index].contains(term))
                .count();
            if matched_terms < required {
                continue;
            }
            hits.push((
                index,
                OracleHit {
                    id: doc.id,
                    exact_phrase: contains_contiguous_terms(&self.tokens[index], &full_terms),
                    matched_terms,
                    score: self.bm25(index, &phrases),
                },
            ));
        }
        // Membership: class, then BM25, then most recently remembered row among exact ties.
        hits.sort_by(|(li, left), (ri, right)| {
            right
                .exact_phrase
                .cmp(&left.exact_phrase)
                .then_with(|| right.matched_terms.cmp(&left.matched_terms))
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| self.docs[*ri].id.cmp(&self.docs[*li].id))
        });
        hits.truncate(limit);
        // Presentation: class, BM25, then the deterministic temporal/provenance chain.
        hits.sort_by(|(li, left), (ri, right)| {
            let (l, r) = (&self.docs[*li], &self.docs[*ri]);
            right
                .exact_phrase
                .cmp(&left.exact_phrase)
                .then_with(|| right.matched_terms.cmp(&left.matched_terms))
                .then_with(|| right.score.total_cmp(&left.score))
                .then_with(|| r.observed_at.cmp(&l.observed_at))
                .then_with(|| r.valid_from.cmp(&l.valid_from))
                .then_with(|| l.content.cmp(&r.content))
                .then_with(|| l.source_uri.cmp(&r.source_uri))
                .then_with(|| l.kind.cmp(&r.kind))
                .then_with(|| l.source_id.cmp(&r.source_id))
                .then_with(|| l.supersedes_id.cmp(&r.supersedes_id))
                .then_with(|| l.id.cmp(&r.id))
        });
        hits.into_iter().take(limit).map(|(_, hit)| hit).collect()
    }
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn input(scope: &str, content: impl Into<String>) -> MemoryInput {
    MemoryInput {
        scope: scope.into(),
        kind: "source".into(),
        content: content.into(),
        source_uri: Some("test://retrieval".into()),
        source_id: None,
        supersedes_id: None,
        observed_at: None,
        valid_from: None,
        valid_to: None,
    }
}

fn insert_all(session: &mut memory::MemorySession, inputs: &[MemoryInput]) -> Vec<i64> {
    let mut ids = Vec::new();
    for chunk in inputs.chunks(memory::MAX_MEMORY_BATCH_RECORDS) {
        ids.extend(
            session
                .remember_batch(chunk)
                .unwrap()
                .into_iter()
                .map(|record| record.id),
        );
    }
    ids
}

fn ids(results: &[memory::MemorySearchResult]) -> Vec<i64> {
    results.iter().map(|result| result.memory.id).collect()
}

fn contents(results: &[memory::MemorySearchResult]) -> Vec<String> {
    results
        .iter()
        .map(|result| result.memory.content.clone())
        .collect()
}

fn assert_oracle_agreement(
    session: &memory::MemorySession,
    oracle: &Oracle,
    scope: &str,
    query: &str,
    limit: usize,
) {
    let expected = oracle.rank(scope, query, FAR_FUTURE, limit);
    #[cfg(feature = "retrieval-experiments")]
    let variants = 0..8;
    #[cfg(not(feature = "retrieval-experiments"))]
    let variants = 0..1;
    for variant in variants {
        #[cfg(feature = "retrieval-experiments")]
        crate::kernel_diagnostic::select(variant);
        #[cfg(not(feature = "retrieval-experiments"))]
        let _ = variant;
        let engine = session
            .recall(scope, query, Some(FAR_FUTURE), limit)
            .unwrap();
        assert_eq!(
            ids(&engine),
            expected.iter().map(|hit| hit.id).collect::<Vec<_>>(),
            "engine/oracle top-{limit} diverged for query {query:?} in {scope}; variant {variant}; oracle: {expected:?}"
        );
    }
    #[cfg(feature = "retrieval-experiments")]
    crate::kernel_diagnostic::select(0);
}

/// Zipf-skewed deterministic corpus across several namespaces with validity windows,
/// supersession-style expiry, and soft deletion.
fn random_corpus(
    seed: u64,
    docs: usize,
    shuffle_insertion: bool,
) -> (TempDir, std::path::PathBuf, Vec<String>) {
    const VOCAB: usize = 70;
    let mut rng = Xorshift(seed);
    let words = (0..VOCAB)
        .map(|i| format!("term{i:02}"))
        .collect::<Vec<_>>();
    let scopes = ["user:ann", "user:bo", "project:cy"];
    let mut inputs = Vec::new();
    for index in 0..docs {
        let length = 4 + rng.below(26);
        let body = (0..length)
            .map(|_| {
                // Skew: small indexes are drawn far more often.
                let pick = rng.below(VOCAB) * rng.below(VOCAB) / VOCAB;
                words[pick].clone()
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut memory = input(
            scopes[index % scopes.len()],
            format!("{body} uniq{index:04}"),
        );
        match rng.below(10) {
            0 => memory.valid_to = Some("2024-06-01T00:00:00Z".into()),
            1 => memory.valid_from = Some("2090-01-01T00:00:00Z".into()),
            2 => memory.observed_at = Some(format!("2025-01-{:02}T00:00:00Z", 1 + rng.below(28))),
            _ => {}
        }
        inputs.push(memory);
    }
    if shuffle_insertion {
        for index in (1..inputs.len()).rev() {
            inputs.swap(index, rng.below(index + 1));
        }
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("random.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let inserted = insert_all(&mut session, &inputs);
    for id in inserted.iter().step_by(17) {
        let doc_scope = inputs[(*id as usize) - 1].scope.clone();
        session.forget(&doc_scope, *id).unwrap();
    }
    let documents = inputs.iter().map(|doc| doc.content.clone()).collect();
    (temp, path, documents)
}

fn random_queries(seed: u64, documents: &[String], count: usize) -> Vec<String> {
    let mut rng = Xorshift(seed);
    let mut queries = Vec::new();
    for _ in 0..count {
        match rng.below(3) {
            // Contiguous slice of a real document: exercises the phrase class.
            0 => {
                let tokens = documents[rng.below(documents.len())]
                    .split(' ')
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let take = 2 + rng.below(3.min(tokens.len() - 1));
                let start = rng.below(tokens.len() - take + 1);
                queries.push(tokens[start..start + take].join(" "));
            }
            // Independent terms, optionally padded with stop words.
            1 => {
                let take = 1 + rng.below(4);
                let mut parts = (0..take)
                    .map(|_| format!("term{:02}", rng.below(70)))
                    .collect::<Vec<_>>();
                if rng.below(2) == 0 {
                    parts.insert(0, "what is the".into());
                }
                queries.push(parts.join(" "));
            }
            _ => {
                let take = 2 + rng.below(3);
                queries.push(
                    (0..take)
                        .map(|_| format!("term{:02}", rng.below(20)))
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
    }
    queries
}

#[test]
fn oracle_bm25_matches_fts5_rank_for_the_executed_expression() {
    let (_temp, path, _) = random_corpus(0xA11CE, 600, false);
    let oracle = Oracle::new(load_corpus(&path));
    let connection = crate::db::open_db(&path).unwrap();
    let mut checked = 0;
    for query in [
        "term03 term07",
        "term01 term02 term09",
        "term05",
        "term00 term04 term11 term30",
    ] {
        let full = normalized_tokens(query);
        let terms = informative_terms(&full);
        let mut phrases = Vec::new();
        if full.len() > 1 {
            phrases.push(full.clone());
        }
        phrases.extend(terms.iter().map(|term| vec![term.clone()]));
        let match_query = memory::memory_match_query("user:ann", &terms, &full);
        let mut statement = connection
            .prepare(
                "SELECT rowid,rank FROM memory_fts WHERE memory_fts MATCH ?1 AND rank MATCH 'bm25(0.0,1.0)'",
            )
            .unwrap();
        let rows = statement
            .query_map([&match_query], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        assert!(!rows.is_empty());
        let by_id = oracle
            .docs
            .iter()
            .enumerate()
            .map(|(index, doc)| (doc.id, index))
            .collect::<HashMap<_, _>>();
        for (rowid, rank) in rows {
            let expected = oracle.bm25(by_id[&rowid], &phrases);
            let relative = ((-rank) - expected).abs() / expected.abs().max(1e-12);
            assert!(
                relative < 1e-9,
                "oracle BM25 {expected} differs from FTS5 {} for row {rowid}, query {query:?}",
                -rank
            );
            checked += 1;
        }
    }
    assert!(
        checked > 500,
        "oracle validation covered too few rows: {checked}"
    );
}

#[test]
fn staged_recall_equals_exhaustive_oracle_on_random_multi_namespace_corpora() {
    let mut total_queries = 0;
    for (seed, shuffled) in [(1u64, false), (2, true), (3, false)] {
        let (_temp, path, documents) = random_corpus(seed * 7919, 700, shuffled);
        let session = memory::MemorySession::open(&path).unwrap();
        let oracle = Oracle::new(load_corpus(&path));
        for (offset, query) in random_queries(seed, &documents, 120).iter().enumerate() {
            let scope = ["user:ann", "user:bo", "project:cy"][offset % 3];
            for limit in [3, 10] {
                assert_oracle_agreement(&session, &oracle, scope, query, limit);
                total_queries += 1;
            }
        }
    }
    assert_eq!(total_queries, 720);
}

#[test]
fn single_term_matches_cannot_crowd_out_documents_containing_both_terms() {
    let mut rng = Xorshift(99);
    let scope = "project:crowding";
    let mut inputs = Vec::new();
    for index in 0..4_000 {
        inputs.push(input(
            scope,
            format!(
                "filler{} filler{} filler{} entry{index}",
                rng.below(300),
                rng.below(300),
                rng.below(300)
            ),
        ));
    }
    // Short, repetitive single-term documents outscore a long two-term document under BM25.
    for index in 0..400 {
        inputs.push(input(
            scope,
            format!("billing billing billing billing billing billing billing billing b{index}"),
        ));
        inputs.push(input(scope, format!("migration migration migration migration migration migration migration migration m{index}")));
    }
    let targets = (0..5)
        .map(|i| {
            format!(
                "target{i} the billing team finished the data cutover and later confirmed the migration \
                 passed every audit check across regions after many long review meetings {}",
                "padding ".repeat(30)
            )
        })
        .collect::<Vec<_>>();
    for target in &targets {
        inputs.push(input(scope, target.clone()));
    }
    for index in (1..inputs.len()).rev() {
        inputs.swap(index, rng.below(index + 1));
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("crowding.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);

    let (legacy, _) = session
        .recall_profiled_ordered(
            scope,
            "billing migration",
            None,
            5,
            RecallCandidateOrder::Ranked,
        )
        .unwrap();
    let legacy_hits = legacy
        .iter()
        .filter(|result| targets.contains(&result.memory.content))
        .count();
    assert!(
        legacy_hits < 5,
        "fixture no longer demonstrates the v0.3.1 window-crowding defect ({legacy_hits}/5 found)"
    );

    let results = session.recall(scope, "billing migration", None, 5).unwrap();
    assert_eq!(results.len(), 5);
    assert!(
        results
            .iter()
            .all(|result| targets.contains(&result.memory.content))
    );
    // Exactly five eligible rows exist, so nothing was left unexamined.
    assert_eq!(results[0].candidate_coverage, CandidateCoverage::Complete);
    let oracle = Oracle::new(load_corpus(&path));
    assert_oracle_agreement(&session, &oracle, scope, "billing migration", 5);
}

#[test]
fn phrase_class_outranks_shorter_documents_with_more_bm25_but_no_phrase() {
    let scope = "project:phrase-class";
    let mut inputs = Vec::new();
    for index in 0..800 {
        inputs.push(input(
            scope,
            format!("release alpha notes beta release gamma notes delta s{index}"),
        ));
    }
    for index in 0..2_000 {
        inputs.push(input(
            scope,
            format!("unrelated filler text number {index}"),
        ));
    }
    let target = format!(
        "the release notes for version nine {}",
        "detail ".repeat(60)
    );
    inputs.insert(1_300, input(scope, target.clone()));
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("phrase.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);
    let results = session.recall(scope, "release notes", None, 10).unwrap();
    assert_eq!(results[0].memory.content, target);
    assert_eq!(results[0].match_reason, "memory_exact_phrase");
    let oracle = Oracle::new(load_corpus(&path));
    assert_oracle_agreement(&session, &oracle, scope, "release notes", 10);
}

#[test]
fn recall_scores_are_invariant_to_insertion_order_and_content_is_invariant_without_ties_at_the_cut()
{
    let build = |reverse: bool, rotate: usize| {
        let mut rng = Xorshift(4242);
        let mut inputs = (0..1_500)
            .map(|index| {
                input(
                    "project:order",
                    format!(
                        "term{:02} term{:02} term{:02} note{index:04}",
                        rng.below(8),
                        rng.below(8),
                        rng.below(30)
                    ),
                )
            })
            .collect::<Vec<_>>();
        inputs.rotate_left(rotate);
        if reverse {
            inputs.reverse();
        }
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("order.sqlite3");
        let mut session = memory::MemorySession::open(&path).unwrap();
        insert_all(&mut session, &inputs);
        let oracle = Oracle::new(load_corpus(&path));
        (temp, session, oracle)
    };
    let variants = [build(false, 0), build(true, 0), build(false, 733)];
    let scores = |index: usize, query: &str| {
        let (_, session, oracle) = &variants[index];
        let results = session.recall("project:order", query, None, 10).unwrap();
        let by_id = oracle
            .rank("project:order", query, FAR_FUTURE, 1_500)
            .into_iter()
            .map(|hit| (hit.id, (hit.exact_phrase, hit.matched_terms, hit.score)))
            .collect::<HashMap<_, _>>();
        let boundary_tie = {
            let ranked = oracle.rank("project:order", query, FAR_FUTURE, 11);
            ranked.len() == 11 && ranked[9].score == ranked[10].score
        };
        (
            results
                .iter()
                .map(|r| {
                    let (phrase, matched, score) = by_id[&r.memory.id];
                    (phrase, matched, (score * 1e9).round() as i64)
                })
                .collect::<Vec<_>>(),
            contents(&results),
            boundary_tie,
        )
    };
    let (mut tie_free, mut tied) = (0, 0);
    for query in [
        "term01 term03",
        "term00 term02 term25",
        "term04 term05 term06",
        "term07 term07 term11",
        "term02 term03",
    ] {
        let (reference_scores, reference_contents, boundary_tie) = scores(0, query);
        assert_eq!(reference_scores.len(), 10);
        for index in 1..variants.len() {
            let (candidate_scores, candidate_contents, _) = scores(index, query);
            assert_eq!(
                candidate_scores, reference_scores,
                "insertion order changed the relevance profile for {query:?}"
            );
            if !boundary_tie {
                assert_eq!(
                    candidate_contents, reference_contents,
                    "insertion order changed top-10 content without a tie at the cut for {query:?}"
                );
            }
        }
        if boundary_tie {
            tied += 1;
        } else {
            tie_free += 1;
        }
    }
    assert!(tie_free + tied == 5);
}

#[test]
fn broad_single_term_is_exact_by_default_and_recency_window_only_when_requested() {
    let scope = "project:broad";
    // Short, older, status-focused evidence amid thousands of longer boilerplate records that
    // all contain the same term (mirrors the frozen `common-status-single-token` dev case).
    let mut inputs = vec![
        input(scope, "status report"),
        input(scope, "where is status"),
    ];
    inputs.extend((0..5_000).map(|index| {
        input(
            scope,
            format!("status background entry with extra boilerplate words {index:05}"),
        )
    }));
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("broad.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let all_ids = insert_all(&mut session, &inputs);
    let oracle = Oracle::new(load_corpus(&path));

    // Default: exact BM25 order. The short record about status ranks first, as in v0.3.1.
    let exact = session.recall(scope, "status", None, 5).unwrap();
    assert_eq!(exact[0].memory.content, "status report");
    assert_eq!(exact[0].candidate_coverage, CandidateCoverage::ExactTopK);
    assert_oracle_agreement(&session, &oracle, scope, "status", 5);

    // Opt-in recent policy: newest rows, explicitly labeled, and it can omit older evidence.
    let recent = session
        .recall_with_policy(scope, "status", None, 5, memory::BroadTermPolicy::Recent)
        .unwrap();
    assert_eq!(
        ids(&recent),
        all_ids.iter().rev().take(5).copied().collect::<Vec<_>>()
    );
    assert!(
        recent
            .iter()
            .all(|r| r.candidate_coverage == CandidateCoverage::RecencyWindow)
    );
    assert!(recent.iter().all(|r| r.memory.content != "status report"));

    // Even in recent mode the exact-phrase record is found; it sits ahead of the window.
    let phrase = session
        .recall_with_policy(
            scope,
            "where is status",
            None,
            5,
            memory::BroadTermPolicy::Recent,
        )
        .unwrap();
    assert_eq!(phrase[0].memory.content, "where is status");
    assert_eq!(phrase[0].match_reason, "memory_exact_phrase");

    // Forgotten and not-yet-valid rows never appear in the recency window.
    session.forget(scope, *all_ids.last().unwrap()).unwrap();
    let after_forget = session
        .recall_with_policy(scope, "status", None, 3, memory::BroadTermPolicy::Recent)
        .unwrap();
    assert_eq!(after_forget[0].memory.id, all_ids[all_ids.len() - 2]);
    let at_past = session
        .recall_with_policy(
            scope,
            "status",
            Some("2000-01-01T00:00:00Z"),
            3,
            memory::BroadTermPolicy::Recent,
        )
        .unwrap();
    assert_eq!(at_past.len(), 3);

    // Small namespaces never use the shortcut, even if requested.
    let small_scope = "project:small";
    session
        .remember(
            small_scope,
            "source",
            "status report small",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let small = session
        .recall_with_policy(
            small_scope,
            "status",
            None,
            3,
            memory::BroadTermPolicy::Recent,
        )
        .unwrap();
    assert_eq!(small[0].candidate_coverage, CandidateCoverage::Complete);
}

#[test]
fn discriminative_single_term_is_ranked_exactly_even_when_it_matches_thousands() {
    let scope = "project:discriminative";
    let mut rng = Xorshift(5);
    let inputs = (0..5_000)
        .map(|index| {
            let hot = rng.below(10) < 2; // roughly 20% of records: broad but discriminative
            input(
                scope,
                format!(
                    "{} filler{} filler{} entry{index}",
                    if hot { "pipeline" } else { "other" },
                    rng.below(50),
                    rng.below(50)
                ),
            )
        })
        .collect::<Vec<_>>();
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("discriminative.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);
    let results = session.recall(scope, "pipeline", None, 10).unwrap();
    assert_eq!(results.len(), 10);
    assert_eq!(results[0].candidate_coverage, CandidateCoverage::ExactTopK);
    let oracle = Oracle::new(load_corpus(&path));
    assert_oracle_agreement(&session, &oracle, scope, "pipeline", 10);
}

#[test]
fn supersession_forget_and_namespace_isolation_hold_under_staged_retrieval() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("temporal.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let alice = "user:alice";
    let bob = "user:bob";
    let source = session
        .remember(
            alice,
            "source",
            "alice chat transcript",
            Some("chat://1"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let first = session
        .remember(
            alice,
            "derived",
            "alice prefers light theme editor",
            Some("chat://1"),
            Some(source.id),
            None,
            None,
            Some("2025-01-01T00:00:00Z"),
            None,
        )
        .unwrap();
    let second = session
        .remember(
            alice,
            "derived",
            "alice prefers dark theme editor",
            Some("chat://2"),
            Some(source.id),
            Some(first.id),
            None,
            Some("2025-06-01T00:00:00Z"),
            None,
        )
        .unwrap();
    session
        .remember(
            bob,
            "source",
            "bob prefers neon theme editor alice prefers dark theme editor",
            Some("chat://3"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let wrong = session
        .remember(
            alice,
            "source",
            "alice prefers theme editor mistaken note",
            Some("chat://4"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let now = session
        .recall(
            alice,
            "alice prefers theme editor",
            Some("2025-07-01T00:00:00Z"),
            5,
        )
        .unwrap();
    let now_ids = ids(&now);
    assert!(now_ids.contains(&second.id) && !now_ids.contains(&first.id));
    let then = session
        .recall(
            alice,
            "alice prefers theme editor",
            Some("2025-03-01T00:00:00Z"),
            5,
        )
        .unwrap();
    assert!(ids(&then).contains(&first.id) && !ids(&then).contains(&second.id));
    assert!(
        now.iter()
            .chain(then.iter())
            .all(|result| result.memory.scope == alice)
    );
    assert_eq!(second.supersedes_id, Some(first.id));
    assert_eq!(
        now.iter()
            .find(|r| r.memory.id == second.id)
            .unwrap()
            .memory
            .source_id,
        Some(source.id)
    );

    assert!(session.forget(alice, wrong.id).unwrap());
    let after = session
        .recall(
            alice,
            "alice prefers theme editor",
            Some("2025-07-01T00:00:00Z"),
            5,
        )
        .unwrap();
    assert!(!ids(&after).contains(&wrong.id));
    // Bob's namespace cannot see Alice's rows, even though one of his rows quotes her.
    let bob_view = session
        .recall(
            bob,
            "alice prefers dark theme editor",
            Some("2025-07-01T00:00:00Z"),
            5,
        )
        .unwrap();
    assert!(bob_view.iter().all(|result| result.memory.scope == bob));

    let oracle = Oracle::new(load_corpus(&path));
    for scope in [alice, bob] {
        assert_oracle_agreement(&session, &oracle, scope, "alice prefers theme editor", 5);
    }
}

#[test]
fn coverage_labels_never_claim_completeness_when_stages_were_skipped() {
    let scope = "project:coverage";
    let mut inputs = (0..40)
        .map(|index| input(scope, format!("alpha beta shared record {index}")))
        .collect::<Vec<_>>();
    inputs.extend((0..6).map(|index| input(scope, format!("alpha gamma only {index}"))));
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("coverage.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);
    // 40 rows satisfy the best class, so lower classes are never enumerated.
    let filled = session.recall(scope, "alpha beta", None, 10).unwrap();
    assert!(
        filled
            .iter()
            .all(|r| r.candidate_coverage == CandidateCoverage::ExactTopK)
    );
    // Asking for more than every eligible row exhausts all stages.
    let exhausted = session.recall(scope, "alpha beta", None, 100).unwrap();
    assert_eq!(exhausted.len(), 40);
    assert!(
        exhausted
            .iter()
            .all(|r| r.candidate_coverage == CandidateCoverage::Complete)
    );
    // More than six informative terms use a bounded BM25 window and say so.
    let long_query = "alpha beta gamma delta epsilon zeta eta theta";
    let bounded = session.recall(scope, long_query, None, 3).unwrap();
    assert!(!bounded.is_empty());
    assert!(bounded.iter().all(|r| matches!(
        r.candidate_coverage,
        CandidateCoverage::Bm25TopK | CandidateCoverage::Complete
    )));
}

struct Judged {
    query: &'static str,
    /// False when the judged answer shares no stemmed/identical terms with the query
    /// (morphology or paraphrase); those are reported but cannot be gated for a lexical engine.
    lexical: bool,
    /// Graded relevance by exact content (2 = needed evidence, 1 = supporting).
    relevant: &'static [(&'static str, u32)],
    /// Rows that must never be returned: stale, forgotten, or from another namespace.
    forbidden: &'static [&'static str],
    at: &'static str,
}

#[test]
fn independently_labeled_long_term_memory_cases_meet_quality_gates() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("judged.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let scope = "user:judged";
    let other = "user:other";
    let src = session
        .remember(
            scope,
            "source",
            "session one transcript: user discussed editor, city and team",
            Some("session://1"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    fn derive(
        session: &mut memory::MemorySession,
        source_id: i64,
        content: &str,
        from: &str,
        supersedes: Option<i64>,
    ) -> memory::MemoryRecord {
        session
            .remember(
                "user:judged",
                "derived",
                content,
                Some("session://1"),
                Some(source_id),
                supersedes,
                None,
                Some(from),
                None,
            )
            .unwrap()
    }
    let mut derived = |content: &str, from: &str, supersedes: Option<i64>| {
        derive(&mut session, src.id, content, from, supersedes)
    };
    let editor_old = derived(
        "the user edits code with vim in the terminal",
        "2024-01-01T00:00:00Z",
        None,
    );
    derived(
        "the user edits code with helix in the terminal",
        "2025-03-01T00:00:00Z",
        Some(editor_old.id),
    );
    let city_old = derived("the user lives in madrid", "2023-01-01T00:00:00Z", None);
    derived(
        "the user lives in lisbon",
        "2025-05-01T00:00:00Z",
        Some(city_old.id),
    );
    derived(
        "the team ships releases every second thursday",
        "2024-02-01T00:00:00Z",
        None,
    );
    derived(
        "el usuario prefiere reuniones por la mañana",
        "2024-02-01T00:00:00Z",
        None,
    );
    derived(
        "the user is allergic to peanuts",
        "2024-02-01T00:00:00Z",
        None,
    );
    let forgotten = derived(
        "the user shared a home alarm code",
        "2024-02-01T00:00:00Z",
        None,
    );
    let _ = &mut derived;
    for index in 0..400 {
        session
            .remember(
                scope,
                "source",
                &format!("status update meeting review project configuration note {index}"),
                Some("session://noise"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }
    session
        .remember(
            other,
            "source",
            "the user lives in oslo and edits code with emacs",
            Some("session://x"),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    session.forget(scope, forgotten.id).unwrap();

    let cases = [
        Judged {
            query: "which editor does the user use for code",
            lexical: true,
            relevant: &[("the user edits code with helix in the terminal", 2)],
            forbidden: &[
                "the user edits code with vim in the terminal",
                "the user lives in oslo and edits code with emacs",
            ],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "which editor does the user use for code",
            lexical: true,
            relevant: &[("the user edits code with vim in the terminal", 2)],
            forbidden: &["the user edits code with helix in the terminal"],
            at: "2024-06-01T00:00:00Z",
        },
        Judged {
            query: "where does the user live",
            lexical: false,
            relevant: &[("the user lives in lisbon", 2)],
            forbidden: &[
                "the user lives in madrid",
                "the user lives in oslo and edits code with emacs",
            ],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "user lives in madrid",
            lexical: true,
            relevant: &[("the user lives in madrid", 2)],
            forbidden: &["the user lives in lisbon"],
            at: "2024-06-01T00:00:00Z",
        },
        Judged {
            query: "when does the team ship releases",
            lexical: true,
            relevant: &[("the team ships releases every second thursday", 2)],
            forbidden: &[],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "reuniones por la mañana",
            lexical: true,
            relevant: &[("el usuario prefiere reuniones por la mañana", 2)],
            forbidden: &[],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "food allergies of the user",
            lexical: false,
            relevant: &[("the user is allergic to peanuts", 1)],
            forbidden: &[],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "home alarm code",
            lexical: true,
            relevant: &[],
            forbidden: &["the user shared a home alarm code"],
            at: "2025-07-01T00:00:00Z",
        },
        Judged {
            query: "quarterly revenue forecast",
            lexical: true,
            relevant: &[],
            forbidden: &[],
            at: "2025-07-01T00:00:00Z",
        },
    ];
    let (mut r1, mut r5, mut r10, mut p5, mut mrr, mut ndcg) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut paraphrase_total, mut paraphrase_found) = (0usize, 0usize);
    let (mut positive, mut abstain_ok, mut abstain_total, mut leaks) =
        (0usize, 0usize, 0usize, 0usize);
    for case in &cases {
        let results = session
            .recall(scope, case.query, Some(case.at), 10)
            .unwrap();
        leaks += results.iter().filter(|r| r.memory.scope != scope).count();
        for result in &results {
            assert!(
                !case.forbidden.contains(&result.memory.content.as_str()),
                "{:?} returned forbidden evidence {:?}",
                case.query,
                result.memory.content
            );
        }
        if case.relevant.is_empty() {
            abstain_total += 1;
            if results.is_empty() || results.iter().all(|r| !r.memory.content.contains("alarm")) {
                abstain_ok += 1;
            }
            continue;
        }
        if !case.lexical {
            paraphrase_total += 1;
            paraphrase_found += results
                .iter()
                .any(|r| case.relevant.iter().any(|(c, _)| *c == r.memory.content))
                as usize;
            continue;
        }
        positive += 1;
        let grade = |content: &str| {
            case.relevant
                .iter()
                .find(|(c, _)| *c == content)
                .map_or(0, |(_, g)| *g)
        };
        let first_hit = results.iter().position(|r| grade(&r.memory.content) > 0);
        let hit_within = |k: usize| first_hit.is_some_and(|p| p < k) as u8 as f64;
        r1 += hit_within(1);
        r5 += hit_within(5);
        r10 += hit_within(10);
        p5 += results
            .iter()
            .take(5)
            .filter(|r| grade(&r.memory.content) > 0)
            .count() as f64
            / 5.0;
        mrr += first_hit.map_or(0.0, |p| 1.0 / (p as f64 + 1.0));
        let dcg: f64 = results
            .iter()
            .take(10)
            .enumerate()
            .map(|(i, r)| {
                (2f64.powi(grade(&r.memory.content) as i32) - 1.0) / (i as f64 + 2.0).log2()
            })
            .sum();
        let mut ideal = case.relevant.iter().map(|(_, g)| *g).collect::<Vec<_>>();
        ideal.sort_by(|a, b| b.cmp(a));
        let idcg: f64 = ideal
            .iter()
            .enumerate()
            .map(|(i, g)| (2f64.powi(*g as i32) - 1.0) / (i as f64 + 2.0).log2())
            .sum();
        ndcg += dcg / idcg;
    }
    let n = positive as f64;
    println!(
        "labeled-suite: cases={} R@1={:.3} R@5={:.3} R@10={:.3} P@5={:.3} MRR={:.3} nDCG@10={:.3} abstention={}/{} namespace_leaks={leaks} paraphrase_or_morphology_found={}/{}",
        cases.len(),
        r1 / n,
        r5 / n,
        r10 / n,
        p5 / n,
        mrr / n,
        ndcg / n,
        abstain_ok,
        abstain_total,
        paraphrase_found,
        paraphrase_total
    );
    assert_eq!(leaks, 0);
    assert_eq!(abstain_ok, abstain_total);
    assert!(r5 / n >= 0.99, "R@5 {}", r5 / n);
    assert!(r10 / n >= 0.99, "R@10 {}", r10 / n);
    assert!(mrr / n >= 0.75, "MRR {}", mrr / n);
}

fn assert_scope_counts_match_rows(connection: &Connection) {
    let mut expected: Vec<(String, i64)> = connection
        .prepare("SELECT scope,count(*) FROM memories WHERE deleted_at IS NULL GROUP BY scope")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let total: i64 = expected.iter().map(|(_, count)| count).sum();
    expected.push((String::new(), total));
    expected.sort();
    let mut stored: Vec<(String, i64)> = connection
        .prepare("SELECT scope,active FROM memory_scope_counts WHERE active > 0 ORDER BY scope")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    stored.sort();
    expected.retain(|(_, count)| *count > 0);
    assert_eq!(stored, expected);
}

#[test]
fn scope_counts_track_remember_forget_batch_and_hard_delete() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("counts.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let inputs = (0..30)
        .map(|i| {
            input(
                ["user:a", "user:b", "project:c"][i % 3],
                format!("count row {i}"),
            )
        })
        .collect::<Vec<_>>();
    let ids = insert_all(&mut session, &inputs);
    let single = session
        .remember(
            "user:a",
            "source",
            "single insert",
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    assert_scope_counts_match_rows(&raw);
    assert!(session.forget("user:a", single.id).unwrap());
    assert!(!session.forget("user:a", single.id).unwrap());
    assert!(session.forget("user:b", ids[1]).unwrap());
    assert_scope_counts_match_rows(&raw);
    raw.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    raw.execute("DELETE FROM memories WHERE id=?1", [ids[2]])
        .unwrap();
    raw.execute("DELETE FROM memories WHERE id=?1", [ids[1]])
        .unwrap(); // already forgotten
    assert_scope_counts_match_rows(&raw);
    // A failing batch rolls back the counters together with the rows.
    let mut bad = vec![input("user:a", "valid before failure")];
    let mut invalid = input("user:a", "invalid link");
    invalid.source_id = Some(9_999_999);
    bad.push(invalid);
    assert!(session.remember_batch(&bad).is_err());
    assert_scope_counts_match_rows(&raw);
}

#[test]
fn schema_three_database_migrates_to_scope_counts_and_keeps_every_memory() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("v3.sqlite3");
    {
        let mut session = memory::MemorySession::open(&path).unwrap();
        let inputs = (0..12)
            .map(|i| {
                input(
                    ["user:a", "user:b"][i % 2],
                    format!("legacy shared fact {i}"),
                )
            })
            .collect::<Vec<_>>();
        insert_all(&mut session, &inputs);
        session.forget("user:a", 1).unwrap();
    }
    // Reconstruct exactly what a v3 binary leaves behind: no counter table or triggers.
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TRIGGER memories_count_insert; DROP TRIGGER memories_count_soft_delete;
         DROP TRIGGER memories_count_delete; DROP TABLE memory_scope_counts; DROP TABLE memory_operations;
         UPDATE metadata SET value='3' WHERE key='schema_version';
         PRAGMA user_version=3;",
    )
    .unwrap();
    let before: i64 = raw
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    drop(raw);

    let original = path;
    let path = temp.path().join("migration-copy.sqlite3");
    std::fs::copy(&original, &path).unwrap();
    let original_bytes = std::fs::read(&original).unwrap();
    // Fail after table creation/backfill, before the schema version is committed.
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER reject_v4 BEFORE UPDATE ON metadata WHEN new.key='schema_version' AND new.value='4' BEGIN SELECT RAISE(ABORT,'injected migration failure'); END;",
    )
    .unwrap();
    drop(raw);
    assert!(crate::db::open_db(&path).is_err());
    let raw = Connection::open(&path).unwrap();
    let state: (i64, bool) = raw.query_row(
        "SELECT (SELECT user_version FROM pragma_user_version), EXISTS(SELECT 1 FROM sqlite_master WHERE name='memory_scope_counts')",
        [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(state, (3, false));
    raw.execute_batch("DROP TRIGGER reject_v4;").unwrap();
    drop(raw);
    let migrated = crate::db::open_db(&path).unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    let integrity: String = migrated
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 5);
    let after: i64 = migrated
        .query_row("SELECT count(*) FROM memories", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    assert_scope_counts_match_rows(&migrated);
    drop(migrated);
    let session = memory::MemorySession::open(&path).unwrap();
    assert_eq!(
        session
            .recall("user:b", "legacy shared fact", None, 10)
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        session
            .recall("user:a", "legacy shared fact", None, 10)
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn skewed_namespaces_use_sql_scope_filter_and_still_match_the_oracle() {
    // 90% of rows are in one namespace, so its FTS expression omits the scope term.
    let mut rng = Xorshift(77);
    let mut inputs = Vec::new();
    for index in 0..1_800 {
        inputs.push(input(
            "user:big",
            format!(
                "term{:02} term{:02} term{:02} fact{index:04}",
                rng.below(12),
                rng.below(12),
                rng.below(40)
            ),
        ));
    }
    for index in 0..200 {
        inputs.push(input(
            "user:small",
            format!(
                "term{:02} term{:02} term{:02} small{index:04}",
                rng.below(12),
                rng.below(12),
                rng.below(40)
            ),
        ));
    }
    for index in (1..inputs.len()).rev() {
        inputs.swap(index, rng.below(index + 1));
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("skew.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);
    let oracle = Oracle::new(load_corpus(&path));
    let mut compared = 0;
    for query in [
        "term01 term02",
        "term03 term04 term30",
        "term05",
        "term00 term07 term11",
    ] {
        for scope in ["user:big", "user:small"] {
            assert_oracle_agreement(&session, &oracle, scope, query, 10);
            let results = session.recall(scope, query, None, 50).unwrap();
            assert!(results.iter().all(|result| result.memory.scope == scope));
            compared += 1;
        }
    }
    assert_eq!(compared, 8);
}

#[test]
#[ignore = "diagnostic: prints query plans"]
fn print_query_plans() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("plan.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let inputs = (0..3000)
        .map(|i| {
            input(
                ["user:a", "user:b", "user:c", "user:d"][i % 4],
                format!("term{} filler{i}", i % 7),
            )
        })
        .collect::<Vec<_>>();
    insert_all(&mut session, &inputs);
    for order in [RecallCandidateOrder::Staged, RecallCandidateOrder::Ranked] {
        println!(
            "{order:?}: {:#?}",
            session
                .recall_query_plan_ordered("user:a", "term3", order)
                .unwrap()
        );
    }
}

#[test]
fn long_queries_use_a_labeled_bounded_window_and_still_surface_phrase_and_full_matches() {
    let scope = "project:long";
    let mut inputs = (0..600)
        .map(|index| input(scope, format!("alpha filler{} text{index}", index % 13)))
        .collect::<Vec<_>>();
    let phrase = "alpha beta gamma delta epsilon zeta";
    inputs.insert(300, input(scope, phrase));
    inputs.insert(
        450,
        input(scope, "zeta epsilon delta gamma beta alpha reordered"),
    );
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("long.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    insert_all(&mut session, &inputs);
    // Six informative terms: more than the per-class staging limit.
    let results = session.recall(scope, phrase, None, 5).unwrap();
    assert_eq!(results[0].memory.content, phrase);
    assert_eq!(results[0].match_reason, "memory_exact_phrase");
    assert!(
        results
            .iter()
            .any(|r| r.memory.content == "zeta epsilon delta gamma beta alpha reordered")
    );
    assert!(results.iter().all(|r| matches!(
        r.candidate_coverage,
        CandidateCoverage::Bm25TopK | CandidateCoverage::Complete
    )));
    assert!(results.iter().all(|r| r.memory.scope == scope));
}

#[test]
fn newest_first_rank_scan_preserves_ties_and_widens_past_ineligible_winners() {
    let scope = "project:descending";
    let mut inputs = Vec::new();
    // Exact BM25 ties at the cut must select newest eligible IDs. Newer short rows from
    // another namespace and expired rows beat them in BM25 and force repeated widening.
    for _ in 0..80 {
        inputs.push(input(scope, "status evidence"));
    }
    for _ in 0..160 {
        inputs.push(input("project:other", "status"));
        let mut expired = input(scope, "status");
        expired.valid_to = Some("2000-01-01T00:00:00Z".into());
        inputs.push(expired);
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("descending.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    let inserted = insert_all(&mut session, &inputs);
    let oracle = Oracle::new(load_corpus(&path));
    for limit in [1, 10, 79, 80, 100] {
        assert_oracle_agreement(&session, &oracle, scope, "status", limit);
        let forced = session
            .recall_ordered(
                scope,
                "status",
                Some(FAR_FUTURE),
                limit,
                RecallCandidateOrder::StagedDescending,
            )
            .unwrap();
        assert_eq!(
            ids(&forced),
            oracle
                .rank(scope, "status", FAR_FUTURE, limit)
                .iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>()
        );
    }
    #[cfg(feature = "retrieval-experiments")]
    crate::kernel_diagnostic::begin();
    let results = session
        .recall(scope, "status", Some(FAR_FUTURE), 10)
        .unwrap();
    #[cfg(feature = "retrieval-experiments")]
    {
        let trace = crate::kernel_diagnostic::finish();
        assert!(trace.fetch_limits.len() > trace.stages);
        assert!(trace.filtered_rows > 0);
        assert!(trace.hydrated >= results.len());
        println!("widening_work={}", serde_json::to_string(&trace).unwrap());
    }
    assert_eq!(ids(&results), inserted[70..80].to_vec());
    assert!(
        results
            .iter()
            .all(|hit| hit.candidate_coverage == CandidateCoverage::ExactTopK)
    );
    let plan = session
        .recall_query_plan_ordered(scope, "status", RecallCandidateOrder::StagedDescending)
        .unwrap();
    assert!(
        plan.iter().any(|step| step.contains("CO-ROUTINE")),
        "{plan:?}"
    );
}

#[test]
fn production_uses_direct_scan_and_both_exact_paths_survive_score_variation_and_mutation() {
    let scope = "project:scan-planner";
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("scan-planner.sqlite3");
    let mut session = memory::MemorySession::open(&path).unwrap();
    // A cheap first-16-score probe sees ties, but cannot predict the later varied scores.
    let mut inputs = (0..16)
        .map(|_| input(scope, "status evidence"))
        .collect::<Vec<_>>();
    for index in 0..600 {
        let mut record = input(
            if index % 4 == 0 {
                "project:other"
            } else {
                scope
            },
            format!(
                "{} {}",
                "status ".repeat(1 + index % 7),
                "padding ".repeat(1 + index % 41)
            ),
        );
        if index % 5 == 0 {
            record.valid_to = Some("2000-01-01T00:00:00Z".into());
        }
        inputs.push(record);
    }
    insert_all(&mut session, &inputs);
    assert_eq!(session.benchmark_score_probe(scope, "status").unwrap(), 1);
    let check = |session: &memory::MemorySession| {
        let oracle = Oracle::new(load_corpus(&path));
        for limit in [1, 10, 100] {
            let expected = oracle
                .rank(scope, "status", FAR_FUTURE, limit)
                .iter()
                .map(|hit| hit.id)
                .collect::<Vec<_>>();
            for order in [
                RecallCandidateOrder::Staged,
                RecallCandidateOrder::StagedDirect,
                RecallCandidateOrder::StagedDescending,
            ] {
                let found = session
                    .recall_ordered(scope, "status", Some(FAR_FUTURE), limit, order)
                    .unwrap();
                assert_eq!(ids(&found), expected, "{order:?}, limit {limit}");
            }
        }
    };
    check(&session);
    let id = session.remember_batch(&[input(scope, "status")]).unwrap()[0].id;
    check(&session);
    assert!(session.forget(scope, id).unwrap());
    check(&session);
    let plan = session
        .recall_query_plan_ordered(scope, "status", RecallCandidateOrder::Staged)
        .unwrap();
    let direct = session
        .recall_query_plan_ordered(scope, "status", RecallCandidateOrder::StagedDirect)
        .unwrap();
    let descending = session
        .recall_query_plan_ordered(scope, "status", RecallCandidateOrder::StagedDescending)
        .unwrap();
    assert_eq!(plan, direct);
    assert_ne!(plan, descending);
}
