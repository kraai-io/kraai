import { metricLabels } from "./format.ts";
import { compareTasks, isFinished, summarize } from "./results.ts";
import { metricKeys } from "./types.ts";
import type { Attempt, MetricKey, Version } from "./types.ts";

export type HistoryMetric = MetricKey | "pass_rate" | "cache_rate";

export interface HistoryPoint {
  version: Version;
  value: number | null;
  samples: number;
  attempts: number;
  tasks: number;
  complete: boolean;
}

export const historyMetrics: { key: HistoryMetric; label: string }[] = [
  { key: "cost_usd", label: "Cost / attempt" },
  { key: "pass_rate", label: "Pass rate" },
  { key: "duration_ms", label: "Runtime / attempt" },
  { key: "cache_rate", label: "Cached input" },
  ...metricKeys.filter((key) => key !== "cost_usd" && key !== "duration_ms")
    .map((key) => ({ key, label: `${metricLabels[key]} / attempt` })),
];

export function historyTasks(attempts: Attempt[], versions: Version[]): string[] {
  const ids = new Set(versions.map(({ id }) => id));
  return [...new Set(attempts.filter((attempt) => ids.has(attempt.version_id) && isFinished(attempt))
    .map(({ task }) => task))].sort((a, b) => a.localeCompare(b));
}

export function comparisonTasks(attempts: Attempt[], versions: Version[]): string[] {
  if (!versions.length) return [];
  return compareTasks(attempts, versions).tasks.filter(({ comparable }) => comparable).map(({ name }) => name);
}

export function historyPoint(
  attempts: Attempt[],
  version: Version,
  tasks: string[],
  metric: HistoryMetric,
): HistoryPoint {
  const groups = new Map([...new Set(tasks)].map((task) => [task, [] as Attempt[]]));
  for (const attempt of attempts) {
    if (attempt.version_id === version.id && isFinished(attempt)) groups.get(attempt.task)?.push(attempt);
  }
  const summaries = [...groups.values()].map(summarize);
  const measurements = summaries.map((summary) => metric === "pass_rate"
    ? { value: summary.finished ? summary.statuses.passed / summary.finished : null, samples: summary.finished }
    : metric === "cache_rate" ? summary.cache : summary.metrics[metric]);
  const covered = summaries.filter(({ finished }) => finished > 0).length;
  const complete = groups.size > 0 && covered === groups.size;
  const measured = measurements.every(({ value }) => value !== null && Number.isFinite(value));
  const mean = complete && measured
    ? measurements.reduce((sum, { value }) => sum + value! / measurements.length, 0)
    : null;
  return {
    version,
    value: mean !== null && Number.isFinite(mean) ? mean : null,
    samples: measurements.reduce((sum, { samples }) => sum + samples, 0),
    attempts: summaries.reduce((sum, { finished }) => sum + finished, 0),
    tasks: covered,
    complete,
  };
}

export function historySeries(
  attempts: Attempt[],
  versions: Version[],
  tasks: string[],
  metric: HistoryMetric,
): HistoryPoint[] {
  const groups = new Map<string, Attempt[]>();
  for (const attempt of attempts) {
    const group = groups.get(attempt.version_id) ?? [];
    group.push(attempt);
    groups.set(attempt.version_id, group);
  }
  return [...versions]
    .sort((a, b) => {
      if (a.latest_at_ms === b.latest_at_ms) return a.id.localeCompare(b.id);
      if (a.latest_at_ms === null) return -1;
      if (b.latest_at_ms === null) return 1;
      return a.latest_at_ms - b.latest_at_ms;
    })
    .map((version) => historyPoint(groups.get(version.id) ?? [], version, tasks, metric));
}
