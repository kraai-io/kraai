from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from harbor.models.agent.context import AgentContext


def _count(value: object, name: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
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


def _estimated_cost(accounting: dict | None) -> float | None:
    if (
        accounting is None
        or accounting.get("model_requests", 0) == 0
        or accounting.get("unpriced_requests") != 0
        or accounting.get("unrecorded_requests") != 0
        or accounting.get("cost_overflow")
    ):
        return None
    return _count(accounting.get("known_estimated_cost"), "estimated cost") / 1_000_000_000


def codex_usage(path: Path) -> dict[str, int] | None:
    result = None
    if not path.is_file():
        return None
    with path.open() as stream:
        for line in stream:
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
    return result


def populate_context(logs_dir: Path, context: AgentContext, controller_dir: Path) -> None:
    metadata = {}
    for name, filename, directory in (
        ("runner", "runner-metrics.json", controller_dir),
        ("harness", "kraai-metrics.json", logs_dir),
        ("proxy", "proxy-metrics.json", controller_dir),
    ):
        path = directory / filename
        if path.is_file():
            metadata[name] = json.loads(path.read_text())
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
        usage = codex_usage(logs_dir / "runner.stdout.jsonl")
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
    context.cost_usd = (
        _estimated_cost(proxy.get("accounting"))
        if proxy is not None
        and proxy.get("unrecorded_requests", 0) == 0
        and proxy.get("accounting_error") is None
        else None
    )
    if context.cost_usd is not None:
        metadata["cost_source"] = "estimated_api_equivalent"
    context.metadata = {"kraai_eval": metadata}
