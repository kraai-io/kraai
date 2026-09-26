import assert from "node:assert/strict";
import test from "node:test";
import { compareTasks, defaultSelection, preserveSelection, summarize } from "../src/results.ts";
import { metric } from "../src/format.ts";
import { metricKeys } from "../src/types.ts";
import type { Attempt, Metrics, Version } from "../src/types.ts";

const versions: Version[] = [
  { id: "current", benchmark: "test", model: "model", harness: "kraai", label: "Current", latest_at_ms: 3 },
  { id: "previous", benchmark: "test", model: "model", harness: "kraai", label: "Previous", latest_at_ms: 1 },
  { id: "codex", benchmark: "test", model: "model", harness: "codex", label: "Codex", latest_at_ms: 2 },
];

function attempt(id: string, overrides: Partial<Attempt> = {}, metrics: Partial<Metrics> = {}): Attempt {
  return {
    id,
    version_id: "current",
    task: "shared",
    attempt: 1,
    status: "passed",
    started_at_ms: null,
    error: null,
    logs: [],
    ...overrides,
    metrics: { ...Object.fromEntries(metricKeys.map((key) => [key, null])) as Metrics, ...metrics },
  };
}

test("pass denominator includes errors, excludes running and interrupted, and keeps all statuses", () => {
  const summary = summarize([
    attempt("passed"),
    attempt("failed", { status: "failed" }),
    attempt("error", { status: "error" }),
    attempt("running", { status: "running" }, { cost_usd: 100 }),
    attempt("interrupted", { status: "interrupted" }, { cost_usd: 100 }),
  ]);
  assert.equal(summary.finished, 3);
  assert.deepEqual(summary.statuses, { passed: 1, failed: 1, error: 1, running: 1, interrupted: 1 });
  assert.equal(summary.metrics.cost_usd.value, null);
});

test("missing metrics do not become zero and recorded zeros remain valid", () => {
  const summary = summarize([attempt("missing"), attempt("zero", {}, { cost_usd: 0 }), attempt("known", {}, { cost_usd: 4 })]);
  assert.deepEqual(summary.metrics.cost_usd, { value: 2, samples: 2 });
  assert.deepEqual(summary.metrics.duration_ms, { value: null, samples: 0 });
  assert.deepEqual(summarize([]).cache, { value: null, samples: 0 });
});

test("cache share weights paired input and cached input observations", () => {
  const summary = summarize([
    attempt("small", {}, { input_tokens: 100, cached_input_tokens: 100 }),
    attempt("large", {}, { input_tokens: 900, cached_input_tokens: 0 }),
    attempt("missing", {}, { input_tokens: 1000 }),
    attempt("invalid", {}, { input_tokens: 50, cached_input_tokens: 100 }),
  ]);
  assert.deepEqual(summary.cache, { value: 0.1, samples: 2 });
});

test("comparison aggregates shared finished tasks while preserving the union table", () => {
  const comparison = compareTasks([
    attempt("a", {}, { cost_usd: 2 }),
    attempt("b", { version_id: "previous" }, { cost_usd: 4 }),
    attempt("extra", { task: "only-current" }, { cost_usd: 100 }),
    attempt("unfinished-a", { task: "unfinished" }, { cost_usd: 50 }),
    attempt("unfinished-b", { task: "unfinished", version_id: "previous", status: "running" }),
    attempt("unselected", { version_id: "codex", task: "not-selected" }),
  ], versions.slice(0, 2));
  assert.equal(comparison.shared, 1);
  assert.deepEqual(comparison.tasks.map((task) => task.name), ["only-current", "shared", "unfinished"]);
  assert.equal(comparison.summaries[0].metrics.cost_usd.value, 2);
  assert.equal(comparison.summaries[1].metrics.cost_usd.value, 4);
  assert.equal(comparison.tasks[0].summaries[1].metrics.cost_usd.value, null);
});

test("defaults choose latest Kraai versions and Codex, polling preserves intentional omissions", () => {
  assert.deepEqual(defaultSelection(versions), ["current", "previous", "codex"]);
  assert.deepEqual(preserveSelection(["previous", "", "codex"], [...versions].reverse()), ["previous", "", "codex"]);
  assert.deepEqual(preserveSelection(["removed", "previous", "codex"], versions), ["", "previous", "codex"]);
});

test("previous defaults to a different build even when current has multiple configurations", () => {
  const duplicate = { ...versions[0], id: "current-other-config", latest_at_ms: 2 };
  assert.deepEqual(defaultSelection([...versions, duplicate]), ["current", "previous", "codex"]);
  assert.deepEqual(defaultSelection([versions[0], duplicate]), ["current", "", ""]);
});

test("unavailable and nonfinite metrics never display as numerical values", () => {
  for (const value of [null, undefined, NaN, Infinity]) assert.equal(metric(value, "cost_usd"), "Unavailable");
  assert.equal(metric(0, "cost_usd"), "$0.00");
});
