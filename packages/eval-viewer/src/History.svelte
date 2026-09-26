<script lang="ts">
  import type { Attempt, Version } from "./types.ts";
  import { isCodex } from "./results.ts";
  import { date, metric as formatMetric, percentage, versionLabel } from "./format.ts";
  import { comparisonTasks, historyMetrics, historyPoint, historySeries, historyTasks } from "./history.ts";
  import type { HistoryMetric } from "./history.ts";
  import TrendChart from "./TrendChart.svelte";

  let { attempts, versions, selected }: { attempts: Attempt[]; versions: Version[]; selected: Version[] } = $props();
  let metric = $state<HistoryMetric>("cost_usd");
  let task = $state("");

  const availableTasks = $derived(historyTasks(attempts, versions));
  const sharedTasks = $derived(comparisonTasks(attempts, selected));
  const tasks = $derived(task ? [task] : sharedTasks);
  const kraaiVersions = $derived(versions.filter((version) => !isCodex(version)));
  const points = $derived(historySeries(attempts, kraaiVersions, tasks, metric));
  const dated = $derived(points.filter((point) => point.version.latest_at_ms !== null));
  const undated = $derived(points.length - dated.length);
  const baseline = $derived(selected.find(isCodex));
  const reference = $derived(baseline ? historyPoint(attempts, baseline, tasks, metric) : undefined);
  const metricLabel = $derived(historyMetrics.find((item) => item.key === metric)?.label ?? metric);

  $effect(() => {
    if (task && !availableTasks.includes(task)) task = "";
  });

  function format(value: number | null): string {
    return metric === "pass_rate" || metric === "cache_rate" ? percentage(value) : formatMetric(value, metric);
  }
</script>

<section aria-label="Result history">
  <div class="heading">
    <h2>History</h2>
    <span class="muted">{kraaiVersions.length} Kraai {kraaiVersions.length === 1 ? "version" : "versions"}</span>
  </div>
  <div class="controls">
    <label>Metric<select aria-label="History metric" bind:value={metric}>{#each historyMetrics as item}<option value={item.key}>{item.label}</option>{/each}</select></label>
    <label>Tasks<select aria-label="History tasks" bind:value={task}><option value="">Comparison tasks · {sharedTasks.length}</option>{#each availableTasks as name}<option value={name}>{name}</option>{/each}</select></label>
  </div>
  <p class="explanation">{task ? "One task" : `${tasks.length} fixed tasks from the selected comparison`}. Each task has equal weight. Versions are ordered by their latest recorded run.</p>

  {#if tasks.length === 0}
    <p class="empty">The selected versions have no finished tasks in common. Choose an individual task to see its history.</p>
  {:else}
    <TrendChart points={dated} {reference} {metric} label={metricLabel} taskCount={tasks.length} />
    {#if reference}
      <p class="baseline"><span class="dash" aria-hidden="true"></span><span>Codex reference · <strong>{format(reference.value)}</strong> · <span title={reference.version.label}>{versionLabel(reference.version)}</span>{#if !reference.complete} · Missing results for this selection{/if}</span></p>
    {:else}
      <p class="footnote">No Codex reference selected.</p>
    {/if}
    {#if undated}<p class="footnote">{undated} {undated === 1 ? "version has" : "versions have"} no recorded run date and {undated === 1 ? "is" : "are"} omitted from the graph.</p>{/if}
    <details>
      <summary>Version details</summary>
      <div class="scroll">
        <table>
          <thead><tr><th>Version</th><th>Latest run</th><th>{metricLabel}</th><th>Samples</th><th>Finished attempts</th><th>Tasks covered</th></tr></thead>
          <tbody>
            {#each points as point (point.version.id)}
              <tr><td class="version-label" title={point.version.label}>{versionLabel(point.version)}</td><td>{date(point.version.latest_at_ms)}</td><td class="number">{format(point.value)}</td><td>{point.samples}</td><td>{point.attempts}</td><td>{point.tasks}/{tasks.length}</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    </details>
    <p class="footnote">Only finished attempts contribute. Missing task results or metrics leave gaps.</p>
  {/if}
</section>

<style>
  .heading { display: flex; align-items: baseline; justify-content: space-between; gap: 16px; }
  .heading span { font-size: 12px; }
  .controls { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 2fr); gap: 14px; margin: 18px 0 10px; }
  label { display: flex; flex-direction: column; gap: 6px; color: var(--muted); font-size: 12px; }
  select { color: #e4e6ed; font-size: 14px; width: 100%; }
  .explanation, .footnote, .baseline { color: var(--muted); font-size: 12px; }
  .explanation { margin-bottom: 20px; }
  .footnote { margin-top: 10px; }
  .baseline { display: flex; gap: 8px; align-items: baseline; margin-top: 13px; overflow-wrap: anywhere; }
  .baseline strong { color: #7fd0b3; font-weight: 500; }
  .dash { display: inline-block; width: 22px; flex-shrink: 0; border-top: 2px dashed #7fd0b3; transform: translateY(-3px); }
  details { margin-top: 18px; }
  summary { cursor: pointer; color: var(--muted); font-size: 12px; width: fit-content; }
  .scroll { margin-top: 10px; }
  th, td { padding: 9px 12px; }
  td { font-size: 12px; }
  th:not(:first-child), td:not(:first-child) { white-space: nowrap; }
  @media (max-width: 600px) { .controls { grid-template-columns: 1fr; } }
</style>
