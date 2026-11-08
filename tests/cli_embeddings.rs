//! CLI-level embedding tests.
//!
//! * Degraded/offline tests always run: no model is needed.
//! * `real_model_*` tests need the multilingual-e5-small weights and an ONNX Runtime library.
//!   Point `AETHER_MIDNIGHT_MODELS_DIR` at a directory populated by `models pull` (and, where the
//!   runtime is not on a standard path, `ORT_DYLIB_PATH`). Without them the test prints `SKIPPED`
//!   to stderr and returns; it is NOT a passing proof of inference (see docs/v0.1-status.md).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn midnight(db: &Path, models: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .args(args)
        .env("AETHER_MIDNIGHT_MODELS_DIR", models)
        // Never read the developer's real configuration.
        .env("AETHER_MIDNIGHT_CONFIG_DIR", db.with_file_name("no-config"))
        .output()
        .unwrap()
}

fn ok_json(db: &Path, models: &Path, args: &[&str]) -> Value {
    let out = midnight(db, models, args);
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?} not JSON: {e}"))
}

#[test]
fn models_list_reports_missing_model_and_never_downloads() {
    let temp = TempDir::new().unwrap();
    let models = temp.path().join("models");
    let db = temp.path().join("m.sqlite3");
    let listed = ok_json(&db, &models, &["models", "list", "--json"]);
    assert_eq!(listed["models"][0]["name"], "multilingual-e5-small");
    assert_eq!(listed["models"][0]["dimensions"], 384);
    assert_eq!(listed["models"][0]["installed"], false);
    // Listing and failing embed must not create the cache (no implicit download or side effects).
    assert!(!models.exists());
    let embed = midnight(&db, &models, &["embed", "--namespace", "proj:a"]);
    assert!(!embed.status.success());
    let stderr = String::from_utf8_lossy(&embed.stderr);
    assert!(
        stderr.contains("models pull multilingual-e5-small"),
        "{stderr}"
    );
    assert!(!models.exists());
    let unknown = midnight(&db, &models, &["models", "pull", "nonexistent-model"]);
    assert!(!unknown.status.success());
}

#[test]
fn lexical_search_and_degraded_hybrid_work_without_any_model() {
    let temp = TempDir::new().unwrap();
    let models = temp.path().join("models");
    let db = temp.path().join("m.sqlite3");
    ok_json(
        &db,
        &models,
        &[
            "memory",
            "add",
            "--namespace",
            "proj:a",
            "the zebra crossing is closed",
            "--json",
        ],
    );

    // Default mode keeps the legacy bare-array contract.
    let legacy = ok_json(
        &db,
        &models,
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "zebra",
            "--json",
        ],
    );
    assert!(legacy.is_array());
    assert_eq!(
        legacy[0]["memory"]["content"],
        "the zebra crossing is closed"
    );
    let explicit = ok_json(
        &db,
        &models,
        &[
            "memory",
            "search",
            "--namespace",
            "proj:a",
            "zebra",
            "--mode",
            "lexical",
            "--json",
        ],
    );
    assert_eq!(legacy, explicit);

    for mode in ["hybrid", "semantic"] {
        let out = midnight(
            &db,
            &models,
            &[
                "memory",
                "search",
                "--namespace",
                "proj:a",
                "zebra",
                "--mode",
                mode,
                "--json",
            ],
        );
        assert!(
            out.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["degraded"], true);
        assert_eq!(v["mode_requested"], mode);
        assert_eq!(v["mode_used"], "lexical");
        assert!(
            v["notice"]
                .as_str()
                .unwrap()
                .contains("semantic search unavailable")
        );
        assert_eq!(
            v["results"][0]["memory"]["content"],
            "the zebra crossing is closed"
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("warning:"));
    }
    assert!(
        !models.exists(),
        "search must never create or download a model cache"
    );
}

#[test]
fn code_search_modes_degrade_to_lexical_and_keep_the_legacy_contract() {
    let temp = TempDir::new().unwrap();
    let models = temp.path().join("models");
    let db = temp.path().join("m.sqlite3");
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("lib.rs"), "pub fn unique_marker_function() {}\n").unwrap();
    let repo_arg = repo.to_str().unwrap();
    ok_json(&db, &models, &["index", "--repo", repo_arg]);
    let legacy = ok_json(
        &db,
        &models,
        &["search", "--repo", repo_arg, "unique_marker_function"],
    );
    assert_eq!(legacy["results"][0]["path"], "lib.rs");
    assert!(legacy.get("degraded").is_none() && legacy.get("mode_used").is_none());
    let hybrid = ok_json(
        &db,
        &models,
        &[
            "search",
            "--repo",
            repo_arg,
            "unique_marker_function",
            "--mode",
            "hybrid",
        ],
    );
    assert_eq!(hybrid["degraded"], true);
    assert_eq!(hybrid["results"][0]["path"], "lib.rs");
    assert_eq!(
        hybrid["results"][0]["line_start"],
        legacy["results"][0]["line_start"]
    );
}

/// Returns the models directory when the real model is installed *and* the runtime works.
fn real_models_dir(db: &Path) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("AETHER_MIDNIGHT_MODELS_DIR")?);
    let listed = ok_json(db, &dir, &["models", "list", "--json"]);
    (listed["models"][0]["installed"] == true).then_some(dir)
}

fn offline(db: &Path, models: &Path, args: &[&str]) -> Output {
    // A dead proxy plus HF offline flags: any network attempt would fail loudly.
    Command::new(env!("CARGO_BIN_EXE_aether-midnight"))
        .arg("--db")
        .arg(db)
        .args(args)
        .env("AETHER_MIDNIGHT_MODELS_DIR", models)
        .env("AETHER_MIDNIGHT_CONFIG_DIR", db.with_file_name("no-config"))
        .env("HF_HUB_OFFLINE", "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .output()
        .unwrap()
}

#[test]
fn real_model_cross_language_hybrid_search_persists_offline() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let Some(models) = real_models_dir(&db) else {
        eprintln!(
            "SKIPPED real_model_cross_language_hybrid_search_persists_offline: multilingual-e5-small not installed (set AETHER_MIDNIGHT_MODELS_DIR)"
        );
        return;
    };
    let add = |ns: &str, text: &str| {
        ok_json(
            &db,
            &models,
            &["memory", "add", "--namespace", ns, text, "--json"],
        )["id"]
            .as_i64()
            .unwrap()
    };
    let gato = add("proj:a", "El gato duerme en el sofá de la sala");
    add(
        "proj:a",
        "Deploy pipeline uses GitHub Actions with a staging gate",
    );
    let factura = add(
        "proj:a",
        "La factura mensual de electricidad subió un 20 por ciento",
    );
    add("proj:b", "The cat sleeps on the living room couch");

    let run = |args: &[&str]| {
        let out = offline(&db, &models, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let embedded = run(&["embed", "--namespace", "proj:a"]);
    assert_eq!(embedded[0]["embedded"], 3);
    assert_eq!(embedded[0]["model"], "multilingual-e5-small");
    // Model reuse + persistence: a second process re-embeds nothing.
    let again = run(&["embed", "--namespace", "proj:a"]);
    assert_eq!(
        (
            again[0]["embedded"].as_i64(),
            again[0]["unchanged"].as_i64()
        ),
        (Some(0), Some(3))
    );

    // English query -> Spanish memory (no shared words), and the reverse direction.
    let hybrid = run(&[
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "a feline resting on the sofa",
        "--mode",
        "hybrid",
        "--json",
    ]);
    assert_eq!(hybrid["degraded"], false);
    assert_eq!(hybrid["mode_used"], "hybrid");
    assert_eq!(hybrid["results"][0]["memory"]["id"], gato);
    let bill = run(&[
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "how much did we pay for power this month",
        "--mode",
        "semantic",
        "--json",
    ]);
    assert_eq!(bill["results"][0]["memory"]["id"], factura);

    // Namespace isolation with a real model: proj:b's cat memory is the closest text but unreachable.
    for r in hybrid["results"].as_array().unwrap() {
        assert_eq!(r["memory"]["scope"], "proj:a");
    }
    // Determinism across processes.
    let repeat = run(&[
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "a feline resting on the sofa",
        "--mode",
        "hybrid",
        "--json",
    ]);
    assert_eq!(hybrid["results"], repeat["results"]);

    // Deletion removes the vector and the result.
    run(&[
        "memory",
        "delete",
        "--namespace",
        "proj:a",
        &gato.to_string(),
        "--json",
    ]);
    let after = run(&[
        "memory",
        "search",
        "--namespace",
        "proj:a",
        "a feline resting on the sofa",
        "--mode",
        "semantic",
        "--json",
    ]);
    assert!(
        after["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["memory"]["id"] != gato)
    );
}

#[test]
fn real_model_code_embedding_search_and_reindex() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let Some(models) = real_models_dir(&db) else {
        eprintln!(
            "SKIPPED real_model_code_embedding_search_and_reindex: multilingual-e5-small not installed (set AETHER_MIDNIGHT_MODELS_DIR)"
        );
        return;
    };
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("auth.rs"), "// Validate the user's password and issue a session token\nfn login(user: &str, password: &str) -> bool { true }\n").unwrap();
    fs::write(
        repo.join("render.rs"),
        "// Draw the bitmap font glyphs onto the screen buffer\nfn draw_glyphs() {}\n",
    )
    .unwrap();
    let repo_arg = repo.to_str().unwrap();
    let run = |args: &[&str]| {
        let out = offline(&db, &models, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    run(&["index", "--repo", repo_arg]);
    assert_eq!(run(&["embed", "--repo", repo_arg])[0]["embedded"], 2);
    let found = run(&[
        "search",
        "--repo",
        repo_arg,
        "inicio de sesión con contraseña",
        "--mode",
        "hybrid",
    ]);
    assert_eq!(found["degraded"], false);
    assert_eq!(found["results"][0]["path"], "auth.rs");
    assert_eq!(found["results"][0]["line_start"], 1);

    fs::write(
        repo.join("auth.rs"),
        "// Render something completely different\nfn other() {}\n",
    )
    .unwrap();
    run(&["index", "--repo", repo_arg]);
    let reembed = run(&["embed", "--repo", repo_arg]);
    assert_eq!(
        (
            reembed[0]["embedded"].as_i64(),
            reembed[0]["unchanged"].as_i64()
        ),
        (Some(1), Some(1))
    );
}
