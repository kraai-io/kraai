import argparse
import asyncio
import json
import shutil
import subprocess
import sys
from pathlib import Path

from kraai_harbor.spec import RunnerSpec


def validate_requested_tasks(requested: list[str], available: set[str]) -> None:
    missing = sorted(set(requested) - available)
    if missing:
        raise ValueError(
            "Task names not found in the selected dataset: " + ", ".join(missing)
        )


async def preflight_tasks(dataset: str, requested: list[str]) -> None:
    if not requested:
        return
    from harbor.registry.client.factory import RegistryClientFactory

    try:
        async with asyncio.timeout(60):
            metadata = await RegistryClientFactory.create().get_dataset_metadata(
                dataset
            )
    except TimeoutError as error:
        raise ValueError("Timed out resolving the selected public dataset") from error
    validate_requested_tasks(requested, {task.get_name() for task in metadata.task_ids})


def build_command(args: argparse.Namespace) -> list[str]:
    if args.attempts < 1:
        raise ValueError("Attempts must be positive")
    if not args.task_name and not args.full_dataset:
        raise ValueError("Select explicit task names or --full-dataset")
    if args.task_name and args.full_dataset:
        raise ValueError("Use task names or --full-dataset, not both")
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
        "--dataset",
        args.dataset,
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
    for name in args.task_name:
        command.extend(("--include-task-name", name))
    for path in args.docker_compose:
        command.extend(("--extra-docker-compose", str(path.resolve(strict=True))))
    for host in args.allow_agent_host:
        command.extend(("--allow-agent-host", host))
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
    parser.add_argument("--docker-compose", action="append", type=Path, default=[])
    parser.add_argument("--allow-agent-host", action="append", default=[])
    parser.add_argument("--print-command", "--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        command = build_command(args)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    if args.print_command:
        print(json.dumps(command, indent=2))
        return
    if args.job_dir.exists():
        parser.error("Job directory already exists; select a new output directory")
    try:
        asyncio.run(preflight_tasks(args.dataset, args.task_name))
    except (ValueError, OSError) as error:
        parser.error(str(error))
    args.job_dir.mkdir(parents=True)
    if args.spec is not None:
        shutil.copyfile(args.spec, args.job_dir / "kraai-eval-spec.json")
    raise SystemExit(subprocess.run(command, check=False).returncode)


if __name__ == "__main__":
    main()
