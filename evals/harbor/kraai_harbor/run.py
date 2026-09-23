import argparse
import asyncio
import json
import subprocess
import signal
import copy
import sys
from pathlib import Path

from kraai_harbor.spec import RunnerSpec
from kraai_harbor.plan import prepare_initial_config, reconcile_job
from kraai_harbor.datasets import (
    default_registry,
    eligible_tasks,
    harbor_task_name,
    order_tasks,
    select_tasks,
)
from kraai_harbor.state import (
    archive_interrupted,
    prepare_run,
    progress,
    run_lock,
    write_json,
)


def validate_requested_tasks(requested: list[str], available: set[str]) -> None:
    missing = sorted(set(requested) - available)
    if missing:
        raise ValueError(
            "Task names not found in the selected dataset: " + ", ".join(missing)
        )


async def preflight_tasks(
    dataset: str, requested: list[str], registry_path: Path | None = None
) -> list[str]:
    from harbor.registry.client.factory import RegistryClientFactory

    try:
        async with asyncio.timeout(60):
            metadata = await RegistryClientFactory.create(
                registry_path=registry_path
            ).get_dataset_metadata(dataset)
    except TimeoutError as error:
        raise ValueError("Timed out resolving the selected public dataset") from error
    available = eligible_tasks(
        dataset, {task.get_name() for task in metadata.task_ids}, requested
    )
    validate_requested_tasks(requested, available)
    return order_tasks(dataset, sorted(requested or available))


def build_command(
    args: argparse.Namespace, task_config: Path | None = None
) -> list[str]:
    if args.attempts < 1:
        raise ValueError("Attempts must be positive")
    count = getattr(args, "task_count", None)
    if count is not None and count < 1:
        raise ValueError("Task count must be positive")
    if not args.task_name and not args.full_dataset and count is None:
        raise ValueError("Select --task-name, --task-count, or --full-dataset")
    if sum((bool(args.task_name), args.full_dataset, count is not None)) > 1:
        raise ValueError("Use exactly one task selection mode")
    if len(set(args.task_name)) != len(args.task_name):
        raise ValueError("Duplicate task names")
    if any(any(character in name for character in "*?[") for name in args.task_name):
        raise ValueError("Task names must be exact names, not glob patterns")
    if "@" not in args.dataset or not args.dataset.rsplit("@", 1)[1]:
        raise ValueError("Public datasets must include an explicit @version")
    if args.oracle == (args.spec is not None):
        raise ValueError(
            "Select --spec for a harness or --oracle for reference solutions"
        )
    spec = RunnerSpec.load(args.spec) if args.spec is not None else None
    if spec is not None and args.model is not None and args.model != spec.model:
        raise ValueError("Requested model must match the runner spec")
    job_dir = args.job_dir.resolve()
    command = [
        sys.executable,
        "-m",
        "harbor.cli.main",
        "run",
        *(["--config", str(task_config)] if task_config else ["--dataset", args.dataset]),
        "--jobs-dir",
        str(job_dir.parent),
        "--job-name",
        job_dir.name,
        "--n-attempts",
        str(args.attempts),
        "--n-concurrent",
        "1",
        "--max-retries",
        "0",
        "--env",
        "docker",
    ]
    if spec is None:
        command.extend(("--agent", "oracle"))
    else:
        command.extend(
            (
                "--agent",
                "kraai_harbor.agent:KraaiAgent"
                if spec.harness == "kraai"
                else "kraai_harbor.agent:ProfileAgent",
                "--model",
                spec.model,
                "--agent-kwarg",
                f"spec_path={job_dir / 'kraai-eval-spec.json'}",
            )
        )
    for name in ([] if task_config else args.task_name):
        command.extend(("--include-task-name", harbor_task_name(args.dataset, name)))
    for path in args.docker_compose:
        command.extend(("--extra-docker-compose", str(path.resolve(strict=True))))
    for host in args.allow_agent_host:
        command.extend(("--allow-agent-host", host))
    if task_config is None and getattr(args, "registry_path", None):
        command.extend(
            ("--registry-path", str(args.registry_path.resolve(strict=True)))
        )
    return command


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--spec", type=Path)
    parser.add_argument("--oracle", action="store_true")
    parser.add_argument("--dataset", required=True)
    parser.add_argument("--job-dir", required=True, type=Path)
    parser.add_argument("--task-name", action="append", default=[])
    parser.add_argument("--full-dataset", action="store_true")
    parser.add_argument("--attempts", type=int, default=1)
    parser.add_argument("--model")
    parser.add_argument("--task-count", type=int)
    parser.add_argument("--registry-path", type=Path)
    parser.add_argument("--status", action="store_true")
    parser.add_argument("--docker-compose", action="append", type=Path, default=[])
    parser.add_argument("--allow-agent-host", action="append", default=[])
    parser.add_argument("--print-command", "--dry-run", action="store_true")
    args = parser.parse_args()
    if args.registry_path is None:
        args.registry_path = default_registry(args.dataset)
    if args.status:
        try:
            manifest = json.loads((args.job_dir / "kraai-run.json").read_text())
            print(
                json.dumps(
                    progress(
                        args.job_dir, manifest["selected_tasks"], manifest["attempts"]
                    ),
                    indent=2,
                )
            )
        except (ValueError, OSError) as error:
            parser.error(str(error))
        return
    try:
        command = build_command(args)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.print_command:
        tasks = asyncio.run(
            preflight_tasks(args.dataset, args.task_name, args.registry_path)
        )
        args.task_name = (
            select_tasks(args.dataset, tasks, args.task_count)
            if args.task_count
            else tasks
        )
        args.task_count = None
        args.full_dataset = False
        command = build_command(args)
        print(json.dumps(command, indent=2))
        return
    try:
        with run_lock(args.job_dir.resolve()) as lock:
            tasks = asyncio.run(
                preflight_tasks(args.dataset, args.task_name, args.registry_path)
            )
            if args.task_count is not None:
                tasks = select_tasks(args.dataset, tasks, args.task_count)
            summary = prepare_run(args, tasks)
            selected = copy.copy(args)
            selected.task_count = None
            selected.full_dataset = False
            selected.task_name = tasks
            if selected.registry_path is not None:
                selected.registry_path = args.job_dir / "kraai-registry.json"
            command = build_command(selected)
            if not summary["remaining"]:
                return
            archive_interrupted(args.job_dir)
            if (args.job_dir / "config.json").exists():
                asyncio.run(reconcile_job(args, tasks))
                command = [
                    sys.executable,
                    "-m",
                    "harbor.cli.main",
                    "jobs",
                    "resume",
                    "--job-path",
                    str(args.job_dir.resolve()),
                ]
            else:
                task_config = asyncio.run(prepare_initial_config(selected, tasks))
                command = build_command(selected, task_config)
            child = subprocess.Popen(
                command, pass_fds=(lock.fileno(),), start_new_session=True
            )
            previous = {
                event: signal.signal(event, lambda *_: child.send_signal(signal.SIGINT))
                for event in (signal.SIGINT, signal.SIGTERM)
            }
            try:
                code = child.wait()
            finally:
                for event, handler in previous.items():
                    signal.signal(event, handler)
                summary = progress(args.job_dir, tasks, args.attempts)
                write_json(args.job_dir / "progress.json", summary)
            raise SystemExit(code)
    except (ValueError, OSError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
