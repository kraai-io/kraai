import unittest
from pathlib import Path

from harbor.job import Job
from harbor.models.trial.config import TaskConfig, TrialConfig


class NativeResumeTests(unittest.TestCase):
    def test_priority_change_reuses_completed_attempts_in_the_new_order(self):
        from kraai_harbor.datasets import TERMINAL_BENCH, order_tasks

        names = ["shadow-relay", "cad-model", "freecad-platform-drawing"]
        completed = {
            name: TrialConfig(task=TaskConfig(path=Path("/tasks") / name), trial_name=name)
            for name in names
        }
        job = object.__new__(Job)
        job._existing_trial_configs = list(completed.values())
        ranked = order_tasks(TERMINAL_BENCH, names)
        job._trial_configs = [
            completed[name].model_copy(update={"trial_name": f"{name}-{attempt}"})
            for name in ranked for attempt in range(2)
        ]
        job._init_remaining_trial_configs()
        self.assertEqual(
            [trial.task.path.name for trial in job._remaining_trial_configs], ranked
        )
        self.assertEqual(len(job._remaining_trial_configs), 3)

    def test_native_reconciliation_preserves_each_completed_attempt(self):
        one = TrialConfig(
            task=TaskConfig(path=Path("/tasks/one")), trial_name="one-old"
        )
        two = TrialConfig(
            task=TaskConfig(path=Path("/tasks/two")), trial_name="two-old"
        )
        job = object.__new__(Job)
        job._existing_trial_configs = [
            one,
            one.model_copy(update={"trial_name": "one-old-2"}),
            two,
        ]
        job._trial_configs = [
            config.model_copy(update={"trial_name": f"new-{index}"})
            for index, config in enumerate([one, two] * 3)
        ]
        job._init_remaining_trial_configs()
        self.assertEqual(len(job._remaining_trial_configs), 3)
        self.assertEqual(
            sum(config.task == one.task for config in job._remaining_trial_configs), 1
        )
        self.assertEqual(
            sum(config.task == two.task for config in job._remaining_trial_configs), 2
        )
        self.assertIs(job._trial_configs[0], one)
        self.assertIs(job._trial_configs[1], two)


class IncrementalPlanTests(unittest.IsolatedAsyncioTestCase):
    async def test_expanding_tasks_attempts_and_subsets_preserves_completed_trials(self):
        import argparse
        import json
        import tempfile
        from unittest.mock import AsyncMock, patch

        from harbor.models.job.config import JobConfig
        from harbor.models.trial.config import AgentConfig
        from harbor.models.trial.result import TrialResult
        from harbor.models.agent.context import AgentContext
        from harbor.models.trial.result import AgentInfo
        from datetime import datetime, timezone
        from kraai_harbor.plan import prepare_initial_config, reconcile_job
        from kraai_harbor.state import archive_interrupted

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tasks = []
            for index in range(10):
                path = root / f"task-{index}"
                path.mkdir()
                (path / "instruction.md").write_text("fixture")
                (path / "task.toml").write_text('version = "1.0"\n')
                (path / "environment").mkdir()
                (path / "environment/Dockerfile").write_text("FROM scratch\n")
                tasks.append(TaskConfig(path=path))
            config = JobConfig(
                job_name="job", jobs_dir=root, tasks=tasks[:5],
                agents=[AgentConfig(name="oracle")],
            )
            args = argparse.Namespace(job_dir=root, dataset="fixture@1", registry_path=None)
            with patch("kraai_harbor.plan.DatasetConfig.get_task_configs", new=AsyncMock(return_value=tasks[:5][::-1])):
                initial = await prepare_initial_config(args, [f"task-{i}" for i in range(5)])
            config.tasks = JobConfig.model_validate_json(initial.read_text()).tasks
            job = await Job.create(config)
            self.assertEqual([trial.task for trial in job._remaining_trial_configs], tasks[:5])
            job._write_job_lock()
            (job.job_dir / "config.json").write_text(config.model_dump_json())
            saved = {}

            def complete(current):
                for trial in current._remaining_trial_configs:
                    path = current.job_dir / trial.trial_name
                    path.mkdir()
                    (path / "config.json").write_text(trial.model_dump_json())
                    result = TrialResult(
                        task_name=trial.task.path.name,
                        task_id=trial.task.get_task_id(),
                        task_checksum="fixture",
                        trial_name=trial.trial_name,
                        trial_uri=path.as_uri(),
                        config=trial,
                        started_at=datetime.now(timezone.utc),
                        finished_at=datetime.now(timezone.utc),
                        agent_info=AgentInfo(name="oracle", version="test"),
                        agent_result=AgentContext(),
                    )
                    (path / "result.json").write_text(result.model_dump_json())
                    saved[path / "result.json"] = (path / "result.json").read_bytes()

            complete(job)
            interrupted = job.job_dir / "interrupted"
            interrupted.mkdir()
            (interrupted / "config.json").write_text("{}")
            (interrupted / "result.json").write_text(json.dumps({
                "task_name": "task-0", "finished_at": None,
                "exception_info": {"exception_type": "CancelledError"},
            }))
            archive_interrupted(job.job_dir)
            self.assertFalse(interrupted.exists())
            args = argparse.Namespace(
                job_dir=job.job_dir, dataset="fixture@1", registry_path=None, attempts=1,
            )
            for count, attempts, remaining in [(10, 1, 5), (10, 2, 10), (10, 2, 0), (5, 1, 0), (5, 3, 5)]:
                args.attempts = attempts
                selected = [f"task-{i}" for i in range(count)]
                with patch("kraai_harbor.plan.DatasetConfig.get_task_configs", new=AsyncMock(return_value=tasks)):
                    await reconcile_job(args, selected)
                updated = JobConfig.model_validate_json((job.job_dir / "config.json").read_text())
                resumed = await Job.create(updated)
                resumed._write_job_lock()
                self.assertEqual(len(resumed._remaining_trial_configs), remaining)
                for path, content in saved.items():
                    self.assertEqual(path.read_bytes(), content)
                complete(resumed)
                resumed._close_logger_handlers()
            (tasks[0].path / "instruction.md").write_text("changed task")
            with patch("kraai_harbor.plan.DatasetConfig.get_task_configs", new=AsyncMock(return_value=tasks)):
                with self.assertRaisesRegex(ValueError, "Saved task inputs changed"):
                    await reconcile_job(args, selected)
            job._close_logger_handlers()


class NamespacedResolutionTests(unittest.IsolatedAsyncioTestCase):
    async def test_preserves_namespaces_and_requested_order(self):
        import argparse
        from unittest.mock import AsyncMock, patch
        from kraai_harbor.plan import resolve_tasks

        tasks = [TaskConfig(name="org-a/task", ref="v1"), TaskConfig(name="org-b/task", ref="v1")]
        args = argparse.Namespace(dataset="org/dataset@v1", registry_path=None)
        with patch("kraai_harbor.plan.DatasetConfig.get_task_configs", new=AsyncMock(return_value=tasks[::-1])):
            resolved = await resolve_tasks(args, ["org-a/task", "org-b/task"])
        self.assertEqual(resolved, tasks)

    async def test_namespaced_completed_attempts_remain_distinct(self):
        from unittest.mock import patch
        from kraai_harbor.plan import attempt_targets
        from kraai_harbor.state import progress

        results = [{"task_name": name, "verifier_result": {"rewards": {"reward": 1}}}
                   for name in ["org-a/task", "org-b/task"]]
        with patch("kraai_harbor.plan.completed_trials", return_value=results):
            targets = attempt_targets(Path("unused"), ["org-a/task"], 2, "org/dataset@v1")
        self.assertEqual(dict(targets), {"org-a/task": 2, "org-b/task": 1})
        with patch("kraai_harbor.state.completed_trials", return_value=results):
            counts = progress(Path("unused"), ["org-a/task", "org-b/task"], 2)
        self.assertEqual(counts["tasks"]["org-a/task"]["remaining"], 1)
        self.assertEqual(counts["tasks"]["org-b/task"]["remaining"], 1)
