import { metricKeys } from "./types.ts";
import type { Attempt, MetricKey, Status, Version } from "./types.ts";

export interface Measurement {
  value: number | null;
  samples: number;
}

export interface Summary {
  attempts: Attempt[];
  statuses: Record<Status, number>;
  finished: number;
  metrics: Record<MetricKey, Measurement>;
  cache: Measurement;
}

export interface TaskGroup {
  name: string;
  summaries: Summary[];
  comparable: boolean;
}

export interface VersionSummary extends Summary {
  tasks: number;
}

export function isFinished(attempt: Attempt): boolean {
  return ["passed", "failed", "error"].includes(attempt.status);
}

export function summarize(attempts: Attempt[]): Summary {
  const statuses: Record<Status, number> = {
    passed: 0,
    failed: 0,
    error: 0,
    running: 0,
    interrupted: 0,
  };
  for (const attempt of attempts) statuses[attempt.status]++;
  const finished = attempts.filter(isFinished);
  const metrics = Object.fromEntries(
    metricKeys.map((key) => {
      const values = finished
        .map((attempt) => attempt.metrics[key])
        .filter((value): value is number => value !== null && Number.isFinite(value));
      return [key, {
        value: values.length ? values.reduce((sum, value) => sum + value, 0) / values.length : null,
        samples: values.length,
      }];
    }),
  ) as Summary["metrics"];
  const cacheSamples = finished.filter(({ metrics }) =>
    metrics.input_tokens !== null && metrics.input_tokens > 0 &&
    metrics.cached_input_tokens !== null && metrics.cached_input_tokens >= 0 &&
    metrics.cached_input_tokens <= metrics.input_tokens,
  );
  const totalInput = cacheSamples.reduce((sum, item) => sum + item.metrics.input_tokens!, 0);
  const cachedInput = cacheSamples.reduce((sum, item) => sum + item.metrics.cached_input_tokens!, 0);
  return {
    attempts,
    statuses,
    finished: finished.length,
    metrics,
    cache: { value: totalInput ? cachedInput / totalInput : null, samples: cacheSamples.length },
  };
}

export function summarizeVersions(attempts: Attempt[], versions: Version[]): Map<string, VersionSummary> {
  const groups = new Map(versions.map(({ id }) => [id, [] as Attempt[]]));
  for (const attempt of attempts) groups.get(attempt.version_id)?.push(attempt);
  return new Map([...groups].map(([id, items]) => [id, {
    ...summarize(items),
    tasks: new Set(items.filter(isFinished).map(({ task }) => task)).size,
  }]));
}

export function compareTasks(attempts: Attempt[], versions: Version[]) {
  const selected = new Set(versions.map(({ id }) => id));
  const groups = new Map<string, Map<string, Attempt[]>>();
  for (const attempt of attempts) {
    if (!selected.has(attempt.version_id)) continue;
    let group = groups.get(attempt.task);
    if (!group) groups.set(attempt.task, group = new Map());
    const items = group.get(attempt.version_id) ?? [];
    items.push(attempt);
    group.set(attempt.version_id, items);
  }
  const tasks: TaskGroup[] = [...groups].sort(([a], [b]) => a.localeCompare(b)).map(([name, group]) => {
    const summaries = versions.map(({ id }) => summarize(group.get(id) ?? []));
    return { name, summaries, comparable: summaries.every((summary) => summary.finished > 0) };
  });
  const shared = tasks.filter((task) => task.comparable);
  const summaries = versions.map((_, index) => summarize(shared.flatMap((task) => task.summaries[index].attempts)));
  return { tasks, shared: shared.length, summaries };
}

export function defaultSelection(versions: Version[]): string[] {
  const sorted = [...versions].sort((a, b) => (b.latest_at_ms ?? 0) - (a.latest_at_ms ?? 0));
  const kraai = sorted.find((version) => !isCodex(version));
  const codex = sorted.find(isCodex);
  return [kraai?.id ?? "", codex?.id ?? ""];
}

export function isCodex(version: Version): boolean {
  return version.harness.toLowerCase().includes("codex");
}

export function preserveSelection(selected: string[], versions: Version[]): string[] {
  const available = new Set(versions.map(({ id }) => id));
  return selected.map((id) => available.has(id) ? id : "");
}
