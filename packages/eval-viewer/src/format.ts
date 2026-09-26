import type { MetricKey, Status, Version } from "./types.ts";
import type { Summary } from "./results.ts";

export const metricLabels: Record<MetricKey, string> = {
  input_tokens: "Input tokens",
  cached_input_tokens: "Cached input tokens",
  uncached_input_tokens: "Uncached input tokens",
  output_tokens: "Output incl. reasoning",
  reasoning_tokens: "Reasoning tokens",
  final_context_tokens: "Final input context",
  turns: "Turns",
  requests: "Requests",
  duration_ms: "Runtime",
  cost_usd: "Estimated API cost",
};

export function metric(value: number | null | undefined, key: MetricKey): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "Unavailable";
  if (key === "cost_usd") return `$${value.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 4 })}`;
  if (key === "duration_ms") {
    const seconds = value / 1000;
    if (seconds < 60) return `${seconds.toFixed(1)}s`;
    if (seconds < 3600) return `${(seconds / 60).toFixed(1)}m`;
    return `${(seconds / 3600).toFixed(1)}h`;
  }
  return value.toLocaleString("en-US", { maximumFractionDigits: 1 });
}

export function percentage(value: number | null): string {
  return value === null ? "Unavailable" : `${(value * 100).toFixed(1)}%`;
}

export function passFraction(summary: Summary): string {
  return summary.finished ? `${summary.statuses.passed}/${summary.finished}` : "Unavailable";
}

export function date(value: number | null): string {
  return value === null ? "Start time unavailable" : new Date(value).toLocaleString();
}

export function versionLabel(version: Version): string {
  const short = version.label.replace(/(?:sha256[:-])?([a-f0-9]{16,})/gi, (_, hash: string) => hash.slice(0, 8));
  const day = version.latest_at_ms === null ? "" : new Date(version.latest_at_ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  return [version.harness, short, day].filter(Boolean).join(" · ");
}

export function statusLabel(status: Status): string {
  return status === "interrupted" ? "unfinished" : status;
}
