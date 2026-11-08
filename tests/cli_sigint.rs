//! Real SIGINT delivered to the CLI binary while it indexes, then database consistency checks.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

fn cli(db: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_aether-midnight"));
    command.arg("--db").arg(db);
    command
}

fn json(db: &Path, args: &[&str]) -> Value {
    let out = cli(db).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn sigint_during_index_exits_130_leaves_consistent_database_and_resumes() {
    let temp = TempDir::new().unwrap();
    let db = temp.path().join("m.sqlite3");
    let repo = temp.path().join("repo");
    for dir in 0..200 {
        let d = repo.join(format!("pkg{dir}"));
        fs::create_dir_all(&d).unwrap();
        for file in 0..100 {
            fs::write(
                d.join(format!("f{file}.rs")),
                format!("pub fn function_{dir}_{file}() -> u32 {{ {file} }}\n// sigint_marker_{dir}_{file}\n"),
            )
            .unwrap();
        }
    }
    let repo_arg = repo.to_str().unwrap();
    json(
        &db,
        &[
            "memory",
            "add",
            "--namespace",
            "proj:a",
            "survives interruption",
            "--json",
        ],
    );

    let mut cancelled_runs = 0;
    for attempt in 0..5u64 {
        let mut child = cli(&db)
            .args(["index", "--repo", repo_arg])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Interrupt once the process is demonstrably working (database file has been created).
        let started = Instant::now();
        std::thread::sleep(Duration::from_millis(250 + attempt * 150));
        unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
        let mut stdout = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        let status = child.wait().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "process did not stop after SIGINT"
        );
        if status.code() == Some(130) {
            let report: Value =
                serde_json::from_str(&stdout).expect("cancelled run still prints a report");
            assert_eq!(report["cancelled"], true);
            // A cancelled run must never claim a complete catalog.
            assert_eq!(report["complete"], Value::Bool(false));
            cancelled_runs += 1;
        } else {
            assert_eq!(status.code(), Some(0), "unexpected exit: {status:?}");
        }

        // Consistency after every interruption.
        let integrity = cli(&db).arg("verify-integrity").output().unwrap();
        assert!(
            integrity.status.success(),
            "{}",
            String::from_utf8_lossy(&integrity.stderr)
        );
        let doctor = json(&db, &["doctor", "--json"]);
        assert_eq!(doctor["ok"], true, "{doctor}");
        let memories = json(&db, &["memory", "list", "--namespace", "proj:a", "--json"]);
        assert_eq!(memories.as_array().unwrap().len(), 1);
        if cancelled_runs > 0 {
            break;
        }
    }
    assert!(
        cancelled_runs > 0,
        "no run was interrupted mid-index; enlarge the fixture"
    );

    // The index resumes and completes; results are exact afterwards.
    let finished = json(&db, &["index", "--repo", repo_arg]);
    assert_eq!(finished["cancelled"], false);
    let hits = json(&db, &["search", "--repo", repo_arg, "sigint_marker_199_99"]);
    assert_eq!(hits["results"][0]["path"], "pkg199/f99.rs");
    let status = json(&db, &["index-status", "--repo", repo_arg]);
    assert_eq!(status["complete"], true, "{status}");
    assert_eq!(json(&db, &["doctor", "--json"])["ok"], true);
}
