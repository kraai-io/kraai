export const metricKeys = [
  "input_tokens",
  "cached_input_tokens",
  "uncached_input_tokens",
  "output_tokens",
  "reasoning_tokens",
  "final_context_tokens",
  "turns",
  "requests",
  "duration_ms",
  "cost_usd",
] as const;

export type MetricKey = (typeof metricKeys)[number];
export type Metrics = Record<MetricKey, number | null>;
export type Status = "passed" | "failed" | "error" | "running" | "interrupted";

export interface Version {
  id: string;
  benchmark: string;
  harness: string;
  model: string | null;
  label: string;
  latest_at_ms: number | null;
}

export interface Attempt {
  id: string;
  version_id: string;
  task: string;
  attempt: number;
  status: Status;
  started_at_ms: number | null;
  metrics: Metrics;
  error: string | null;
  logs: string[];
}

export interface Catalog {
  versions: Version[];
  attempts: Attempt[];
  warnings: string[];
}

export interface AttemptLog {
  name: string;
  content: string;
  truncated: boolean;
}
