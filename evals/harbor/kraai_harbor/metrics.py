from __future__ import annotations

import json
import os
import stat
from collections.abc import Iterator
from pathlib import Path
from typing import TYPE_CHECKING, BinaryIO

if TYPE_CHECKING:
    from harbor.models.agent.context import AgentContext

METRICS_LIMIT = 8 * 1024 * 1024
_USAGE_FIELDS = (
    "total_tokens",
    "input_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "output_tokens",
    "reasoning_tokens",
)
_HARNESS_FIELDS = ("schema_version", "turns")
_NO_FOLLOW = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK


def _open_scoped(root: Path, relative: str) -> BinaryIO | None:
    parts = Path(relative).parts
    if not parts or any(part in (".", "..") or "/" in part for part in parts):
        raise ValueError(f"Invalid artifact path {relative!r}")
    try:
        directory = os.open(root, _NO_FOLLOW | os.O_DIRECTORY)
    except FileNotFoundError:
        return None
    try:
        for part in parts[:-1]:
            try:
                child = os.open(part, _NO_FOLLOW | os.O_DIRECTORY, dir_fd=directory)
            except FileNotFoundError:
                return None
            os.close(directory)
            directory = child
        try:
            descriptor = os.open(parts[-1], _NO_FOLLOW, dir_fd=directory)
        except FileNotFoundError:
            return None
    finally:
        os.close(directory)
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise ValueError(f"{root / relative} is not a regular file")
    return os.fdopen(descriptor, "rb")


def _read_json(root: Path, relative: str) -> object | None:
    stream = _open_scoped(root, relative)
    if stream is None:
        return None
    with stream:
        contents = stream.read(METRICS_LIMIT + 1)
    if len(contents) > METRICS_LIMIT:
        raise ValueError(f"{root / relative} exceeds the {METRICS_LIMIT} byte limit")
    return json.loads(contents)


def _is_count(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _harness_metrics(value: object) -> dict:
    if not isinstance(value, dict):
        raise ValueError("Invalid harness metrics")
    metrics = {
        key: value[key]
        for key in _HARNESS_FIELDS
        if key in value and (value[key] is None or _is_count(value[key]))
    }
    usage = value.get("usage")
    if isinstance(usage, dict):
        metrics["usage"] = {
            key: usage[key] for key in _USAGE_FIELDS if _is_count(usage.get(key))
        }
    return metrics


def _count(value: object, name: str) -> int:
    if not _is_count(value):
        raise ValueError(f"Invalid {name} token count")
    return value


def _normalized_usage(usage: dict) -> dict[str, int]:
    fields = (
        "total_tokens",
        "input_tokens",
        "cache_read_tokens",
        "output_tokens",
        "reasoning_tokens",
    )
    normalized = {field: _count(usage.get(field), field) for field in fields}
    normalized["cache_write_tokens"] = _count(
        usage.get("cache_write_tokens", 0), "cache write"
    )
    return normalized


def _complete_proxy_usage(proxy: dict) -> bool:
    if proxy.get("unrecorded_requests", 0) != 0 or proxy.get("accounting_error") is not None:
        return False
    accounting = proxy.get("accounting")
    if accounting is None:
        return True
    if accounting.get("unrecorded_requests") != 0:
        return False
    model_requests = accounting.get("model_requests", 0)
    if model_requests == 0:
        return proxy.get("requests") == 0
    return accounting.get("context", {}).get("samples") == model_requests


def _lines(stream: BinaryIO) -> Iterator[bytes | None]:
    while line := stream.readline(METRICS_LIMIT + 1):
        if len(line) <= METRICS_LIMIT:
            yield line
            continue
        while line and not line.endswith(b"\n"):
            line = stream.readline(METRICS_LIMIT + 1)
        yield None


def codex_usage(directory: Path, relative: str) -> tuple[dict[str, int] | None, bool]:
    result = None
    complete = True
    stream = _open_scoped(directory, relative)
    if stream is None:
        return None, complete
    with stream:
        for line in _lines(stream):
            if line is None:
                complete = False
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict) or event.get("type") != "turn.completed":
                continue
            usage = event.get("usage")
            if not isinstance(usage, dict):
                continue
            raw_input = _count(usage.get("input_tokens"), "input")
            raw_output = _count(usage.get("output_tokens"), "output")
            cache = _count(usage.get("cached_input_tokens", 0), "cached input")
            reasoning = _count(
                usage.get("reasoning_output_tokens", 0), "reasoning output"
            )
            if cache > raw_input or reasoning > raw_output:
                raise ValueError("Codex token subdivisions exceed their totals")
            current = {
                "input_tokens": raw_input - cache,
                "cache_read_tokens": cache,
                "output_tokens": raw_output - reasoning,
                "reasoning_tokens": reasoning,
                "total_tokens": raw_input + raw_output,
            }
            if result is None:
                result = current
            else:
                result = {key: result[key] + current[key] for key in result}
    return result, complete


def populate_context(logs_dir: Path, context: AgentContext, controller_dir: Path) -> None:
    metadata = {}
    for name, filename, directory in (
        ("runner", "runner-metrics.json", controller_dir),
        ("harness", "kraai-metrics.json", logs_dir),
        ("proxy", "proxy-metrics.json", controller_dir),
    ):
        value = _read_json(directory, filename)
        if value is not None:
            metadata[name] = _harness_metrics(value) if name == "harness" else value
    usage = None
    usage_complete = True
    proxy = metadata.get("proxy")
    harness_usage = metadata.get("harness", {}).get("usage")
    if proxy is not None:
        metadata["usage_source"] = "proxy"
        proxy_usage = proxy.get("usage")
        if proxy_usage is not None:
            usage = _normalized_usage(proxy_usage)
        usage_complete = _complete_proxy_usage(proxy) and (
            proxy.get("requests") == 0 or usage is not None and any(usage.values())
        )
    elif harness_usage:
        usage = _normalized_usage(harness_usage)
        metadata["usage_source"] = "harness"
    else:
        usage, usage_complete = codex_usage(logs_dir, "runner.stdout.jsonl")
        if usage is not None:
            metadata["usage_source"] = "codex_turn_completed"
    if usage is not None:
        metadata["usage"] = usage
        metadata["usage_complete"] = usage_complete
        if usage_complete:
            context.n_input_tokens = (
                usage["input_tokens"]
                + usage["cache_read_tokens"]
                + usage.get("cache_write_tokens", 0)
            )
            context.n_cache_tokens = usage["cache_read_tokens"]
            context.n_output_tokens = usage["output_tokens"] + usage["reasoning_tokens"]
    if usage is None or not usage_complete:
        context.n_input_tokens = None
        context.n_cache_tokens = None
        context.n_output_tokens = None
    metadata["wall_clock_reliable"] = False
    context.cost_usd = None
    context.metadata = {"kraai_eval": metadata}
