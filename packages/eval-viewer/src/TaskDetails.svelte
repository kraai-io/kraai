<script lang="ts">
  import { metricKeys } from "./types.ts";
  import type { Version } from "./types.ts";
  import type { TaskGroup } from "./results.ts";
  import { metric, metricLabels, percentage, statusLabel, versionLabel } from "./format.ts";
  import AttemptDetail from "./AttemptDetail.svelte";

  let { task, versions }: { task: TaskGroup; versions: Version[] } = $props();
  let openAttempt = $state("");
  const attempts = $derived(task.summaries.flatMap((summary, index) => summary.attempts.map((attempt) => ({ attempt, version: versions[index] }))));
  const active = $derived(attempts.find(({ attempt }) => attempt.id === openAttempt)?.attempt);
</script>

<div class="details">
  <div class="intro"><h3>Metrics per finished attempt</h3><p class="muted">Means of recorded values. Each cell shows its sample count.</p></div>
  <div class="scroll">
    <table aria-label={`${task.name} metric comparison`}>
      <thead><tr><th>Metric</th>{#each versions as version}<th class="version-label" title={version.label}>{versionLabel(version)}</th>{/each}</tr></thead>
      <tbody>
        {#each metricKeys as key}
          <tr><th>{metricLabels[key]}</th>{#each task.summaries as summary}<td>{metric(summary.metrics[key].value, key)}<span class="sample">{summary.metrics[key].samples}/{summary.finished} recorded</span></td>{/each}</tr>
        {/each}
        <tr><th>Cached input share</th>{#each task.summaries as summary}<td>{percentage(summary.cache.value)}<span class="sample">{summary.cache.samples}/{summary.finished} recorded</span></td>{/each}</tr>
      </tbody>
    </table>
  </div>
  <h3 class="attempt-heading">Attempts <span class="muted">{attempts.length}</span></h3>
  <div class="scroll">
    <table aria-label={`${task.name} attempts`}>
      <thead><tr><th>Attempt</th><th>Version</th><th>Status</th><th>Cost</th><th>Runtime</th></tr></thead>
      <tbody>
        {#each attempts as { attempt, version } (attempt.id)}
          <tr class:active={openAttempt === attempt.id}>
            <td><button aria-expanded={openAttempt === attempt.id} onclick={() => openAttempt = openAttempt === attempt.id ? "" : attempt.id}>Attempt {attempt.attempt}</button></td>
            <td class="version-label" title={version.label}>{versionLabel(version)}</td>
            <td><span class:passed={attempt.status === "passed"} class:failed={attempt.status === "failed" || attempt.status === "error"}>{statusLabel(attempt.status)}</span></td>
            <td class="number">{metric(attempt.metrics.cost_usd, "cost_usd")}</td>
            <td class="number">{metric(attempt.metrics.duration_ms, "duration_ms")}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  {#if active}{#key active.id}<AttemptDetail attempt={active} />{/key}{/if}
</div>

<style>
  .details { background: #13161d; border-top: 1px solid var(--border); border-bottom: 1px solid var(--border); }
  .intro { padding: 18px 20px 12px; }
  .intro p { font-size: 12px; margin-top: 4px; }
  th, td { padding: 9px 20px; }
  thead th { font-weight: 600; }
  .attempt-heading { padding: 20px 20px 8px; }
  .attempt-heading span { font-weight: 400; padding-left: 4px; }
  button { padding: 4px 8px; white-space: nowrap; font-size: 12px; }
  .passed { color: #9ad5b5; }
  .failed { color: #eea7a7; }
  .active { background: #242938; }
</style>
