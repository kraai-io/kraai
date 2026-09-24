import hashlib
import json
from pathlib import Path


TERMINAL_BENCH = "terminal-bench/terminal-bench@4.0.0"


def default_registry(dataset: str) -> Path | None:
    if dataset == TERMINAL_BENCH:
        return Path(__file__).parent / "datasets/terminal-bench-4.0.0.json"
    return None


def disabled_tasks(dataset: str) -> dict[str, str]:
    path = default_registry(dataset)
    return json.loads(path.read_text())[0]["disabled_tasks"] if path else {}


def eligible_tasks(dataset: str, available: set[str], requested: list[str]) -> set[str]:
    disabled = disabled_tasks(dataset)
    rejected = sorted(set(requested) & disabled.keys())
    if rejected:
        raise ValueError("Tasks are temporarily disabled: " + ", ".join(rejected))
    return available - disabled.keys()


def select_tasks(dataset: str, tasks: list[str], count: int) -> list[str]:
    if not 1 <= count <= len(tasks):
        raise ValueError(f"Task count must be between 1 and {len(tasks)}")
    if dataset == TERMINAL_BENCH:
        return order_tasks(dataset, tasks)[:count]
    return sorted(
        tasks, key=lambda task: hashlib.sha256(f"{dataset}/{task}".encode()).digest()
    )[:count]


def order_tasks(dataset: str, tasks: list[str]) -> list[str]:
    if dataset != TERMINAL_BENCH:
        return list(tasks)
    path = Path(__file__).parent / "datasets/terminal-bench-4.0.0-priority.json"
    stats = json.loads(path.read_text())["tasks"]

    def priority(task):
        if task not in stats:
            return (1, 0, 0, task)
        result = stats[task]
        seconds = result["mean_trial_seconds"]
        rate = result["passes"] / result["attempts"]
        return (0, -rate / seconds, seconds, task)

    return sorted(tasks, key=priority)


def harbor_task_name(dataset: str, task: str) -> str:
    return f"terminal-bench/{task}" if dataset == TERMINAL_BENCH else task
