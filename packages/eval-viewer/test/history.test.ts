import assert from "node:assert/strict";
import test from "node:test";
import { comparisonTasks, historyMetrics, historyPoint, historySeries, historyTasks } from "../src/history.ts";
import { metricKeys } from "../src/types.ts";
import type { Attempt, Metrics, Version } from "../src/types.ts";

const version: Version = {
  id: "kraai",
  benchmark: "test",
  model: "model",
  harness: "kraai",
  label: "build",
  latest_at_ms: 10,
};
const codex: Version = { ...version, id: "codex", harness: "codex" };

function attempt(id: string, overrides: Partial<Attempt> = {}, metrics: Partial<Metrics> = {}): Attempt {
  return {
    id,
    version_id: version.id,
    task: "a",
    attempt: 1,
    status: "passed",
    started_at_ms: null,
    error: null,
    logs: [],
    ...overrides,
    metrics: { ...Object.fromEntries(metricKeys.map((key) => [key, null])) as Metrics, ...metrics },
  };
}

test("history defaults to cost and offers each metric once", () => {
  assert.equal(historyMetrics[0].key, "cost_usd");
  assert.equal(new Set(historyMetrics.map(({ key }) => key)).size, metricKeys.length + 2);
});

test("history task choices include only finished attempts in selected versions", () => {
  const attempts = [
    attempt("z", { task: "z" }),
    attempt("a"),
    attempt("a2"),
    attempt("running", { task: "running", status: "running" }),
    attempt("cancelled", { task: "cancelled", status: "interrupted" }),
    attempt("other", { task: "other", version_id: "other" }),
  ];
  assert.deepEqual(historyTasks(attempts, [version]), ["a", "z"]);
  assert.deepEqual(historyTasks(attempts, []), []);
});

test("comparison cohort is shared finished tasks or all selected Kraai tasks without Codex", () => {
  const attempts = [
    attempt("shared"),
    attempt("kraai-only", { task: "b" }),
    attempt("codex-shared", { version_id: codex.id }),
    attempt("codex-running", { version_id: codex.id, task: "b", status: "running" }),
    attempt("codex-only", { version_id: codex.id, task: "c" }),
  ];
  assert.deepEqual(comparisonTasks(attempts, [version, codex]), ["a"]);
  assert.deepEqual(comparisonTasks(attempts, [version]), ["a", "b"]);
  assert.deepEqual(comparisonTasks(attempts, []), []);
});

test("task means have equal weight despite uneven attempt counts", () => {
  const attempts = [
    attempt("a1", {}, { cost_usd: 2 }),
    attempt("a2", {}, { cost_usd: 4 }),
    attempt("b", { task: "b", status: "failed" }, { cost_usd: 9 }),
  ];
  assert.deepEqual(historyPoint(attempts, version, ["a", "b"], "cost_usd"), {
    version, value: 6, samples: 3, attempts: 3, tasks: 2, complete: true,
  });
  assert.equal(historyPoint(attempts, version, ["a", "b"], "pass_rate").value, 0.5);
});

test("cache rates pair observations within each task then weight tasks equally", () => {
  const attempts = [
    attempt("a1", {}, { input_tokens: 100, cached_input_tokens: 100 }),
    attempt("a2", {}, { input_tokens: 900, cached_input_tokens: 0 }),
    attempt("b", { task: "b" }, { input_tokens: 10, cached_input_tokens: 10 }),
    attempt("missing", {}, { input_tokens: 1000 }),
    attempt("invalid", {}, { input_tokens: 100, cached_input_tokens: 101 }),
  ];
  const point = historyPoint(attempts, version, ["a", "b"], "cache_rate");
  assert.equal(point.value, 0.55);
  assert.equal(point.samples, 3);
  assert.equal(point.attempts, 5);
});

test("a missing metric on one task leaves a gap without silently changing the cohort", () => {
  const point = historyPoint([
    attempt("known", {}, { cost_usd: 2 }),
    attempt("missing", { task: "b" }),
  ], version, ["a", "b"], "cost_usd");
  assert.deepEqual(point, { version, value: null, samples: 1, attempts: 2, tasks: 2, complete: true });
});

test("known metric observations remain usable with explicit sample counts", () => {
  const point = historyPoint([
    attempt("missing"),
    attempt("known", {}, { cost_usd: 2 }),
  ], version, ["a"], "cost_usd");
  assert.equal(point.value, 2);
  assert.equal(point.samples, 1);
  assert.equal(point.attempts, 2);
});

test("unfinished and unselected attempts cannot complete a missing cohort task", () => {
  const attempts = [
    attempt("known", {}, { cost_usd: 2 }),
    attempt("running", { task: "b", status: "running" }, { cost_usd: 100 }),
    attempt("cancelled", { task: "b", status: "interrupted" }, { cost_usd: 100 }),
    attempt("other", { task: "b", version_id: codex.id }, { cost_usd: 100 }),
    attempt("outside", { task: "outside" }, { cost_usd: 100 }),
  ];
  assert.deepEqual(historyPoint(attempts, version, ["a", "b"], "cost_usd"), {
    version, value: null, samples: 1, attempts: 1, tasks: 1, complete: false,
  });
  assert.deepEqual(historyPoint(attempts, version, [], "cost_usd"), {
    version, value: null, samples: 0, attempts: 0, tasks: 0, complete: false,
  });
});

test("zero metrics are valid and failed or error attempts stay in the denominator", () => {
  const attempts = [
    attempt("pass", {}, { cost_usd: 0 }),
    attempt("fail", { status: "failed" }),
    attempt("error", { status: "error" }),
  ];
  assert.equal(historyPoint(attempts, version, ["a"], "cost_usd").value, 0);
  assert.equal(historyPoint(attempts, version, ["a"], "pass_rate").value, 1 / 3);
  assert.equal(historyPoint([attempt("no-input", {}, { input_tokens: 0, cached_input_tokens: 0 })], version, ["a"], "cache_rate").value, null);
});

test("unknown and nonfinite metric observations do not become plotted values", () => {
  for (const value of [null, NaN, Infinity, -Infinity]) {
    assert.equal(historyPoint([attempt("invalid", {}, { cost_usd: value })], version, ["a"], "cost_usd").value, null);
  }
});

test("history preserves missing points and sorts dates with deterministic ties", () => {
  const unknown = { ...version, id: "unknown", latest_at_ms: null };
  const zero = { ...version, id: "zero", latest_at_ms: 0 };
  const tie = { ...version, id: "a-tie" };
  const versions = [version, zero, unknown, tie];
  const series = historySeries([attempt("known", {}, { cost_usd: 2 })], versions, ["a"], "cost_usd");
  assert.deepEqual(series.map(({ version }) => version.id), ["unknown", "zero", "a-tie", "kraai"]);
  assert.deepEqual(series.map(({ value }) => value), [null, null, null, 2]);
  assert.deepEqual(versions.map(({ id }) => id), ["kraai", "zero", "unknown", "a-tie"]);
});

test("duplicate task names do not increase a task's weight", () => {
  const attempts = [attempt("a", {}, { cost_usd: 2 }), attempt("b", { task: "b" }, { cost_usd: 4 })];
  assert.equal(historyPoint(attempts, version, ["a", "a", "b"], "cost_usd").value, 3);
});
