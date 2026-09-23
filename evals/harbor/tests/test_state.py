import argparse
import json

from fixtures import Fixture
from kraai_harbor.datasets import (
    TERMINAL_BENCH,
    default_registry,
    eligible_tasks,
    order_tasks,
    select_tasks,
)
from kraai_harbor.state import archive_interrupted, prepare_run, progress, run_lock


class StateTests(Fixture):
    def setUp(self):
        super().setUp()
        self.args = argparse.Namespace(
            spec=self.spec_path,
            oracle=False,
            dataset="test@1",
            model=None,
            job_dir=self.root / "job",
            attempts=2,
            task_name=["one", "two"],
            full_dataset=False,
            docker_compose=[],
            allow_agent_host=[],
            registry_path=None,
        )
        self.tasks = ["one", "two"]

    def trial(self, name, task, reward=1, error=None, finished=True):
        path = self.args.job_dir / name
        path.mkdir(parents=True)
        (path / "config.json").write_text("{}")
        (path / "result.json").write_text(
            json.dumps(
                {
                    "task_name": task,
                    "finished_at": "2026-09-20T00:00:00Z" if finished else None,
                    "verifier_result": {"rewards": {"reward": reward}},
                    "exception_info": {"exception_type": error} if error else None,
                }
            )
        )
        return path

    def test_resume_counts_passes_failures_and_errors_but_not_cancellation(self):
        prepare_run(self.args, self.tasks)
        self.trial("one-pass", "one")
        self.trial("one-fail", "one", reward=0)
        self.trial("two-error", "two", error="AgentTimeoutError")
        cancelled = self.trial("two-cancelled", "two", error="CancelledError")
        summary = prepare_run(self.args, self.tasks)
        self.assertEqual(summary["completed"], 3)
        self.assertEqual(summary["remaining"], 1)
        self.assertEqual(summary["tasks"]["one"]["failed"], 1)
        self.assertEqual(summary["tasks"]["two"]["errors"], 1)
        archive_interrupted(self.args.job_dir)
        self.assertFalse(cancelled.exists())
        self.assertEqual(len(list(self.root.glob("job.interrupted/*"))), 1)
        self.assertTrue((self.args.job_dir / "one-fail/result.json").exists())

    def test_additional_attempts_preserve_completed_trial_bytes(self):
        prepare_run(self.args, self.tasks)
        saved = self.trial("one-pass", "one") / "result.json"
        before = saved.read_bytes()
        self.args.attempts = 3
        summary = prepare_run(self.args, self.tasks)
        self.assertEqual(summary["remaining"], 5)
        self.assertEqual(saved.read_bytes(), before)
        self.args.attempts = 1
        self.assertEqual(prepare_run(self.args, self.tasks)["remaining"], 1)

    def test_task_selection_extends_and_can_shrink_without_losing_results(self):
        prepare_run(self.args, self.tasks)
        saved = self.trial("one-pass", "one") / "result.json"
        before = saved.read_bytes()
        self.args.task_name = ["one", "two", "three"]
        summary = prepare_run(self.args, self.args.task_name)
        self.assertEqual(summary["remaining"], 5)
        self.args.task_name = ["three"]
        self.assertEqual(prepare_run(self.args, ["three"])["remaining"], 2)
        self.assertEqual(saved.read_bytes(), before)
        manifest = json.loads((self.args.job_dir / "kraai-run.json").read_text())
        self.assertEqual(manifest["tasks"], ["one", "two", "three"])

    def test_modified_configuration_is_rejected_before_altering_results(self):
        prepare_run(self.args, self.tasks)
        for field, value in [
            ("model", "different"),
            ("dataset", "test@2"),
        ]:
            old = getattr(self.args, field)
            setattr(self.args, field, value)
            with (
                self.subTest(field=field),
                self.assertRaisesRegex(ValueError, "differs"),
            ):
                prepare_run(self.args, self.tasks)
            setattr(self.args, field, old)

    def test_corrupt_results_are_not_silently_rerun(self):
        prepare_run(self.args, self.tasks)
        path = self.trial("one", "one") / "result.json"
        path.write_text("{")
        with self.assertRaisesRegex(ValueError, "refusing to rerun"):
            progress(self.args.job_dir, self.tasks, 2)

    def test_wrong_shaped_results_are_rejected_without_moving_or_changing_them(self):
        from kraai_harbor.plan import attempt_targets

        prepare_run(self.args, self.tasks)
        path = self.trial("one", "one") / "result.json"
        valid = json.loads(path.read_text())
        invalid = [None, [], 1, "result", {}, {"finished_at": "2026-09-20"}]
        for field, values in {
            "task_name": [None, 1, [], "", "tasks/"],
            "finished_at": [False, 1, {}, ""],
            "exception_info": [[], "error", False, {"exception_type": []}],
            "verifier_result": [[], "result", False, {"rewards": []}, {"rewards": 0}],
        }.items():
            invalid.extend({**valid, field: value} for value in values)
        for result in invalid:
            with self.subTest(result=result):
                path.write_text(json.dumps(result))
                before = path.read_bytes()
                for action in (
                    lambda: progress(self.args.job_dir, self.tasks, 2),
                    lambda: attempt_targets(self.args.job_dir, self.tasks, 2),
                    lambda: archive_interrupted(self.args.job_dir),
                ):
                    with self.assertRaises(ValueError) as caught:
                        action()
                    self.assertIn(str(path), str(caught.exception))
                    self.assertIn("refusing to rerun", str(caught.exception))
                    self.assertEqual(path.read_bytes(), before)
                self.assertFalse(self.root.joinpath("job.interrupted").exists())

    def test_adapter_initialization_failure_is_archived_without_counting_an_attempt(self):
        prepare_run(self.args, self.tasks)
        trial = self.args.job_dir / "initialization-failed"
        trial.mkdir()
        (trial / "lock.json").write_text("{}")
        (trial / "trial.log").write_text("adapter initialization failed")
        archive_interrupted(self.args.job_dir)
        self.assertFalse(trial.exists())
        archived = list(self.root.glob("job.interrupted/*/trial.log"))
        self.assertEqual(len(archived), 1)
        self.assertEqual(archived[0].read_text(), "adapter initialization failed")
        self.assertEqual(progress(self.args.job_dir, self.tasks, 2)["completed"], 0)

    def test_lock_prevents_concurrent_run_and_releases_after_error(self):
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            with run_lock(self.args.job_dir):
                with self.assertRaisesRegex(ValueError, "already running"):
                    with run_lock(self.args.job_dir):
                        self.fail("second lock acquired")
                raise RuntimeError("interrupted")
        with run_lock(self.args.job_dir):
            pass

    def test_completed_run_has_no_remaining_work(self):
        self.args.attempts = 1
        prepare_run(self.args, self.tasks)
        self.trial("one", "one")
        self.trial("two", "two", reward=0)
        self.assertEqual(prepare_run(self.args, self.tasks)["remaining"], 0)

    def test_task_selection_is_stable_and_extends_by_prefix(self):
        tasks = ["one", "two", "three", "four"]
        first = select_tasks("test@1", tasks, 2)
        self.assertEqual(first, select_tasks("test@1", tasks[::-1], 2))
        self.assertEqual(first, select_tasks("test@1", tasks, 3)[:2])
        for count in [0, 5]:
            with self.assertRaises(ValueError):
                select_tasks("test@1", tasks, count)

    def test_gpu_tasks_are_disabled_before_sampling(self):
        path = default_registry(TERMINAL_BENCH)
        registry = json.loads(path.read_text())[0]
        available = {task["name"] for task in registry["tasks"]}
        eligible = eligible_tasks(TERMINAL_BENCH, available, [])
        self.assertEqual(len(available), 66)
        self.assertEqual(len(eligible), 62)
        self.assertFalse(
            set(select_tasks(TERMINAL_BENCH, list(eligible), 62))
            & set(registry["disabled_tasks"])
        )
        with self.assertRaisesRegex(ValueError, "temporarily disabled"):
            eligible_tasks(TERMINAL_BENCH, available, ["jax-speedrun-gpu"])

    def test_codex_priority_covers_release_and_extends_by_prefix(self):
        registry = default_registry(TERMINAL_BENCH)
        tasks = [task["name"] for task in json.loads(registry.read_text())[0]["tasks"]]
        stats = json.loads(registry.with_name("terminal-bench-4.0.0-priority.json").read_text())["tasks"]
        self.assertEqual(set(stats), set(tasks))
        self.assertEqual(sum(row["attempts"] for row in stats.values()), 330)
        self.assertEqual(sum(row["passes"] for row in stats.values()), 167)
        for row in stats.values():
            self.assertEqual(row["attempts"], 5)
            self.assertGreater(row["mean_trial_seconds"], 0)
            self.assertTrue(0 <= row["passes"] <= row["attempts"])
        eligible = list(eligible_tasks(TERMINAL_BENCH, set(tasks), []))
        first = select_tasks(TERMINAL_BENCH, eligible, 5)
        self.assertEqual(first, [
            "freecad-platform-drawing", "cad-model", "hof-topology-interpenetration",
            "embedding-drift-monitor", "shadow-relay",
        ])
        self.assertEqual(first, select_tasks(TERMINAL_BENCH, eligible[::-1], 10)[:5])
        ranked = order_tasks(TERMINAL_BENCH, tasks + ["unknown-task"])
        self.assertEqual(ranked[-1], "unknown-task")
        for faster in tasks:
            for slower in tasks:
                if (stats[faster]["passes"] >= stats[slower]["passes"]
                    and stats[faster]["mean_trial_seconds"] < stats[slower]["mean_trial_seconds"]):
                    self.assertLess(ranked.index(faster), ranked.index(slower))

    def test_long_running_task_is_disabled_before_sampling(self):
        with self.assertRaisesRegex(ValueError, "temporarily disabled"):
            eligible_tasks(TERMINAL_BENCH, {"ctr-optimization"}, ["ctr-optimization"])
