"""Regression checks for the bounded local and Modal benchmark harnesses."""

from __future__ import annotations

import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from scripts import measure_process, modal_bench, performance_campaign


class PerformanceToolTests(unittest.TestCase):
    def test_distribution_keeps_sorted_nearest_index_samples(self) -> None:
        actual = performance_campaign.distribution([5, 1, 4, 2, 3], "ms")
        self.assertEqual(actual["samples_ms"], [1, 2, 3, 4, 5])
        self.assertEqual(actual["p50_ms"], 3)
        self.assertEqual(actual["p95_ms"], 5)
        self.assertEqual(actual["p99_ms"], 5)

    def test_process_sampler_terminates_a_child_at_its_deadline(self) -> None:
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
