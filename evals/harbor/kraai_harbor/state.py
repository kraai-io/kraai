import fcntl
import hashlib
import json
import os
import uuid
from contextlib import contextmanager
from pathlib import Path


def write_json(path: Path, value: object) -> None:
    temporary = path.with_suffix(".tmp")
    with temporary.open("w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


@contextmanager
def run_lock(job: Path):
    job.parent.mkdir(parents=True, exist_ok=True)
    with job.with_name(job.name + ".lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError(f"Benchmark is already running: {job}") from error
        yield lock


def file_digest(path: Path) -> str:
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def registry_digest(path: Path) -> str:
    registry = json.loads(path.read_text())
    for dataset in registry:
        dataset.pop("disabled_tasks", None)
    return hashlib.sha256(json.dumps(registry, sort_keys=True).encode()).hexdigest()


def run_identity(args) -> dict:
    return {
        "schema_version": 2,
        "dataset": args.dataset,
        "oracle": args.oracle,
        "model": args.model,
        "spec": json.loads(args.spec.read_text()) if args.spec else None,
        "docker_compose": [
            {"path": str(path.resolve()), "sha256": file_digest(path)}
            for path in args.docker_compose
        ],
        "allow_agent_host": sorted(args.allow_agent_host),
        "registry_sha256": registry_digest(args.registry_path)
        if getattr(args, "registry_path", None)
        else None,
    }


def load_saved_result(path: Path) -> dict:
    try:
        result = json.loads(path.read_text())
        if not isinstance(result, dict):
            raise ValueError("expected an object")
        name = result.get("task_name")
        if not isinstance(name, str) or not name.split("/")[-1].strip():
            raise ValueError("expected a nonempty task_name")
        finished = result.get("finished_at")
        if finished is not None and (not isinstance(finished, str) or not finished.strip()):
            raise ValueError("expected a timestamp or null for finished_at")
        for field in ("exception_info", "verifier_result"):
            value = result.get(field)
            if value is not None and not isinstance(value, dict):
                raise ValueError(f"expected an object or null for {field}")
        exception_type = (result.get("exception_info") or {}).get("exception_type")
        if exception_type is not None and not isinstance(exception_type, str):
            raise ValueError("expected a string for exception_type")
        rewards = (result.get("verifier_result") or {}).get("rewards")
        if rewards is not None and not isinstance(rewards, dict):
            raise ValueError("expected an object or null for rewards")
        return result
    except (ValueError, OSError) as error:
        raise ValueError(
            f"Cannot read saved trial {path}; refusing to rerun it: {error}"
        ) from error


def completed_trials(job: Path) -> list[dict]:
    results = []
    for path in sorted(job.glob("*/result.json")):
        result = load_saved_result(path)
        exception = result.get("exception_info") or {}
        if (
            not result.get("finished_at")
            or exception.get("exception_type") == "CancelledError"
        ):
            continue
        results.append(result)
    return results


def progress(job: Path, tasks: list[str], attempts: int) -> dict:
    counts = {
        task: {"completed": 0, "passed": 0, "failed": 0, "errors": 0} for task in tasks
    }
    for result in completed_trials(job):
        task = result["task_name"]
        if task not in counts:
            task = task.removeprefix("terminal-bench/")
        if task not in counts:
            continue
        count = counts[task]
        count["completed"] += 1
        reward = (result.get("verifier_result") or {}).get("rewards") or {}
        if result.get("exception_info"):
            count["errors"] += 1
        elif reward.get("reward") == 1:
            count["passed"] += 1
        else:
            count["failed"] += 1
    for count in counts.values():
        count["remaining"] = max(0, attempts - count["completed"])
    return {
        "attempts_per_task": attempts,
        "completed": sum(count["completed"] for count in counts.values()),
        "remaining": sum(count["remaining"] for count in counts.values()),
        "tasks": counts,
    }


def archive_interrupted(job: Path) -> None:
    for trial in sorted(job.iterdir()):
        if not trial.is_dir() or not any(
            (trial / name).exists() for name in ("config.json", "lock.json")
        ):
            continue
        path = trial / "result.json"
        result = load_saved_result(path) if path.exists() else None
        exception = (result or {}).get("exception_info") or {}
        if (
            result
            and result.get("finished_at")
            and exception.get("exception_type") != "CancelledError"
        ):
            continue
        archive = job.with_name(job.name + ".interrupted")
        archive.mkdir(exist_ok=True)
        trial.rename(archive / f"{trial.name}-{uuid.uuid4().hex}")


def prepare_run(args, tasks: list[str]) -> dict:
    job = args.job_dir.resolve()
    identity = run_identity(args)
    manifest_path = job / "kraai-run.json"
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text())
        if manifest["identity"] != identity:
            raise ValueError(
                "Saved benchmark configuration differs; use a new --output-dir"
            )
    else:
        if job.exists() and any(job.iterdir()):
            raise ValueError(
                "Existing directory has no Kraai run manifest; use a new output directory"
            )
        job.mkdir(parents=True, exist_ok=True)
        manifest = {"identity": identity, "tasks": tasks, "attempts": args.attempts}
    manifest["tasks"] = list(dict.fromkeys(manifest["tasks"] + tasks))
    manifest["selected_tasks"] = tasks
    manifest["attempts"] = args.attempts
    if args.spec:
        frozen = job / "kraai-eval-spec.json"
        if frozen.exists() and json.loads(frozen.read_text()) != identity["spec"]:
            raise ValueError("Saved runner spec was changed; refusing to resume")
        write_json(frozen, identity["spec"])
    if getattr(args, "registry_path", None):
        frozen_registry = job / "kraai-registry.json"
        if (
            frozen_registry.exists()
            and registry_digest(frozen_registry) != identity["registry_sha256"]
        ):
            raise ValueError("Saved dataset registry was changed; refusing to resume")
        if not frozen_registry.exists():
            temporary = frozen_registry.with_suffix(".tmp")
            temporary.write_bytes(args.registry_path.read_bytes())
            temporary.replace(frozen_registry)
    summary = progress(job, tasks, args.attempts)
    write_json(manifest_path, manifest)
    return summary
