"""Regression checks for the bounded local and Modal benchmark harnesses."""

from __future__ import annotations

import contextlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from scripts import evaluate_memory_retrieval, measure_process, modal_bench, performance_campaign


class PerformanceToolTests(unittest.TestCase):
    """Check benchmark helpers and their failure handling."""

    def test_wal_probe_rejects_older_sqlite_before_executing_pragma(self) -> None:
        """Reject unsupported SQLite before issuing the WAL pragma."""
        connection = Mock()
        with patch.object(sqlite3, "sqlite_version_info", (3, 50, 4)):
            with self.assertRaisesRegex(RuntimeError, r"3\.51\.0.*3\.50\.4"):
                evaluate_memory_retrieval.wal_checkpoint_noop(connection)
        connection.execute.assert_not_called()

    def test_evaluator_rejects_older_sqlite_before_starting_server(self) -> None:
        """Fail before starting MCP when the Python SQLite is too old."""
        stderr = io.StringIO()
        with (
            patch.object(sqlite3, "sqlite_version_info", (3, 50, 4)),
            patch.object(sys, "argv", ["evaluate_memory_retrieval.py"]),
            patch.object(evaluate_memory_retrieval, "McpClient") as client,
            contextlib.redirect_stderr(stderr),
        ):
            with self.assertRaises(SystemExit) as exit_error:
                evaluate_memory_retrieval.main()
        self.assertEqual(exit_error.exception.code, 2)
        self.assertIn("requires Python SQLite 3.51.0 or newer", stderr.getvalue())
        client.assert_not_called()

    def test_wal_probe_reports_progress_on_supported_sqlite(self) -> None:
        """Return WAL frame counts when NOOP is supported."""
        connection = Mock()
        connection.execute.return_value.fetchone.return_value = (0, 12, 5)
        with patch.object(sqlite3, "sqlite_version_info", (3, 51, 0)):
            result = evaluate_memory_retrieval.wal_checkpoint_noop(connection)
        connection.execute.assert_called_once_with("PRAGMA wal_checkpoint(NOOP)")
        self.assertEqual(
            result,
            {"busy": 0, "log_frames": 12, "checkpointed_frames": 5, "uncheckpointed_frames": 7},
        )

    @unittest.skipUnless(sqlite3.sqlite_version_info >= (3, 51, 0), "requires SQLite 3.51.0+")
    def test_wal_probe_does_not_checkpoint_live_wal(self) -> None:
        """Confirm repeated NOOP probes leave uncheckpointed WAL progress unchanged."""
        with tempfile.TemporaryDirectory(prefix="midnight-wal-probe-") as temporary:
            database = Path(temporary) / "memory.sqlite3"
            with contextlib.closing(sqlite3.connect(database)) as writer:
                writer.execute("PRAGMA journal_mode=WAL")
                writer.execute("PRAGMA wal_autocheckpoint=0")
                writer.execute("CREATE TABLE records (value TEXT)")
                writer.execute("INSERT INTO records VALUES ('example')")
                writer.commit()
                with contextlib.closing(sqlite3.connect(database)) as probe:
                    first = evaluate_memory_retrieval.wal_checkpoint_noop(probe)
                    second = evaluate_memory_retrieval.wal_checkpoint_noop(probe)
                    self.assertGreater(first["uncheckpointed_frames"], 0)
                    self.assertEqual(first, second)

    def test_distribution_keeps_sorted_nearest_index_samples(self) -> None:
        """Keep sorted samples and nearest-index percentile values."""
        actual = performance_campaign.distribution([5, 1, 4, 2, 3], "ms")
        self.assertEqual(actual["samples_ms"], [1, 2, 3, 4, 5])
        self.assertEqual(actual["p50_ms"], 3)
        self.assertEqual(actual["p95_ms"], 5)
        self.assertEqual(actual["p99_ms"], 5)

    def test_process_sampler_terminates_a_child_at_its_deadline(self) -> None:
        """Terminate a measured subprocess when it exceeds its time limit."""
        result = measure_process.run_measured(
            [sys.executable, "-c", "import time; time.sleep(2)"],
            10,
            None,
            0,
            [],
            0.05,
        )
        self.assertTrue(result["timed_out"])
        self.assertNotEqual(result["returncode"], 0)
        self.assertLess(result["elapsed_ms"], 1000)

    def test_incomplete_structural_index_reports_bounded_parser_paths(self) -> None:
        """Limit reported parser-error paths and flag when results are truncated."""
        with tempfile.TemporaryDirectory(prefix="midnight-parse-errors-") as temporary:
            database = Path(temporary) / "index.sqlite3"
            with sqlite3.connect(database) as connection:
                connection.execute(
                    "CREATE TABLE code_files(rel_path TEXT NOT NULL, parse_error INTEGER NOT NULL)"
                )
                connection.executemany(
                    "INSERT INTO code_files(rel_path, parse_error) VALUES(?, ?)",
                    [("src/a.rs", 1), ("src/b.rs", 1), ("src/c.rs", 0)],
                )
            result = performance_campaign.parse_error_diagnostics(database, limit=1)
        self.assertEqual(result["parse_error_files"], 2)
        self.assertEqual(result["reported_paths"], ["src/a.rs"])
        self.assertTrue(result["truncated"])

    def test_modal_cost_plan_rejects_a_budget_below_reserved_cost(self) -> None:
        """Mark a campaign plan over budget when its reserve exceeds the budget."""
        with patch.object(
            sys,
            "argv",
            ["modal_bench.py", "--mode", "smoke", "--tier", "constrained", "--budget-usd", "1"],
        ):
            args = modal_bench.parse_args()
        plan = modal_bench.campaign_plan(args)
        self.assertFalse(plan["within_budget"])
        self.assertEqual(plan["sample_count_per_worker_configuration"], 1)
        self.assertIsNone(plan["gpu"])

    def test_modal_cost_plan_counts_reservations_from_prior_artifacts(self) -> None:
        """Include previous campaign reservations in the current cost plan."""
        with tempfile.TemporaryDirectory(prefix="midnight-modal-budget-") as temporary:
            output_root = Path(temporary)
            prior_run = output_root / "prior-run"
            prior_run.mkdir()
            (prior_run / "result.json").write_text(
                json.dumps({"plan": {"max_estimated_total_usd": 4.99}}), encoding="utf-8"
            )
            with patch.object(
                sys,
                "argv",
                [
                    "modal_bench.py",
                    "--mode",
                    "smoke",
                    "--tier",
                    "constrained",
                    "--budget-usd",
                    "5",
                    "--image-build-reserve-usd",
                    "0",
                    "--output-root",
                    str(output_root),
                ],
            ):
                args = modal_bench.parse_args()
            plan = modal_bench.campaign_plan(args)
        self.assertEqual(plan["prior_campaign_reservations_usd"], 4.99)
        self.assertFalse(plan["within_budget"])

    def test_modal_result_marks_an_incomplete_remote_measurement(self) -> None:
        """Mark result artifacts as failed when a remote measurement failed."""
        with tempfile.TemporaryDirectory(prefix="midnight-result-test-") as temporary:
            run_dir = Path(temporary)
            modal_bench.save_result(
                run_dir,
                {"mode": "smoke", "tiers": [{"name": "minimal"}]},
                [{"measurement_status": "failed", "measurement_error": "no worker result"}],
            )
            result = json.loads((run_dir / "result.json").read_text())
        self.assertEqual(result["status"], "completed_with_failures")


if __name__ == "__main__":
    unittest.main()
