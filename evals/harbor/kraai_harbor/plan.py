import json
from collections import Counter

from harbor.models.job.config import DatasetConfig, JobConfig
from harbor.models.job.lock import JobLock, build_job_lock
from harbor.models.trial.config import TrialConfig
from harbor.tasks.client import TaskClient

from kraai_harbor.datasets import harbor_task_name, order_tasks
from kraai_harbor.state import completed_trials, write_json


def attempt_targets(job, selected, attempts):
    targets = Counter(
        result["task_name"].split("/")[-1] for result in completed_trials(job)
    )
    for name in selected:
        targets[name] = max(targets[name], attempts)
    return targets


async def resolve_tasks(args, names):
    name, version = args.dataset.rsplit("@", 1)
    dataset = DatasetConfig(
        name=name,
        ref=version if "/" in name else None,
        version=version if "/" not in name else None,
        registry_path=args.registry_path,
        task_names=[harbor_task_name(args.dataset, task) for task in names],
    )
    resolved = await dataset.get_task_configs()
    by_name = {task.get_task_id().get_name().split("/")[-1]: task for task in resolved}
    if set(by_name) != set(names):
        raise ValueError("Could not resolve every saved and requested task")
    return [by_name[name] for name in names]


async def prepare_initial_config(args, selected):
    tasks = await resolve_tasks(args, selected)
    path = args.job_dir.resolve() / "kraai-plan.json"
    write_json(path, {"tasks": [task.model_dump(mode="json") for task in tasks]})
    return path


async def reconcile_job(args, selected):
    job = args.job_dir.resolve()
    targets = attempt_targets(job, selected, args.attempts)
    config_path = job / "config.json"
    config = JobConfig.model_validate_json(config_path.read_text())
    names = order_tasks(args.dataset, list(targets))
    resolved = await resolve_tasks(args, names)
    config.datasets = []
    config.n_attempts = 1
    config.tasks = [
        task for name, task in zip(names, resolved) for _ in range(targets[name])
    ]
    fields = {
        key: value
        for key, value in config.model_dump().items()
        if key in TrialConfig.model_fields
    }
    trials = [
        TrialConfig(**fields, task=task, agent=agent, trials_dir=job)
        for task in config.tasks
        for agent in config.agents
    ]
    ids = [task.get_task_id() for task in resolved]
    downloads = await TaskClient().download_tasks(task_ids=ids)
    lock = build_job_lock(
        config=config,
        trial_configs=trials,
        task_download_results=dict(zip(ids, downloads.results)),
    )
    path = job / "lock.json"
    if path.exists():
        old = JobLock.model_validate_json(path.read_text())
        lock.created_at = old.created_at
        lock.harbor = old.harbor
        previous = {trial.task.name: trial for trial in old.trials}
        for trial in lock.trials:
            if trial.task.name in previous and trial != previous[trial.task.name]:
                raise ValueError(f"Saved task inputs changed: {trial.task.name}")
    for path in job.glob("*/config.json"):
        result = path.with_name("result.json")
        if not result.exists():
            continue
        saved = TrialConfig.model_validate_json(path.read_text())
        if saved not in trials:
            raise ValueError(f"Saved trial configuration changed: {path.parent.name}")
    write_json(job / "lock.json", json.loads(lock.model_dump_json(exclude_none=True)))
    write_json(config_path, json.loads(config.model_dump_json(exclude_none=True)))
