<script lang="ts">
  import type { Version } from "./types.ts";
  import type { Summary } from "./results.ts";
  import { metric, passFraction, percentage, versionLabel } from "./format.ts";

  let { versions, summaries, shared, total }: {
    versions: Version[];
    summaries: Summary[];
    shared: number;
    total: number;
  } = $props();
</script>

<section aria-label="Comparison summary">
  <div class="heading">
    <h2>Comparison</h2>
    <span class="muted">{shared} of {total} tasks shared</span>
  </div>
  <p class="explanation">Only tasks with finished attempts in every selected version. Pass fraction includes passed, failed, and error attempts. Running and unfinished attempts are excluded from averages.</p>
  <div class="cards">
    {#each versions as version, index (version.id)}
      {@const summary = summaries[index]}
      <article>
        <h3 class="version-label" title={version.label}>{versionLabel(version)}</h3>
        <span class="muted">{version.harness} · {summary.finished} finished attempts</span>
        <div class="measures">
          <div><span class="label">Pass fraction</span><strong>{passFraction(summary)}</strong><small class="muted">{percentage(summary.finished ? summary.statuses.passed / summary.finished : null)}</small></div>
          <div><span class="label">Cost / attempt</span><strong>{metric(summary.metrics.cost_usd.value, "cost_usd")}</strong><small class="muted">{summary.metrics.cost_usd.samples} recorded</small></div>
          <div><span class="label">Runtime / attempt</span><strong>{metric(summary.metrics.duration_ms.value, "duration_ms")}</strong><small class="muted">{summary.metrics.duration_ms.samples} recorded</small></div>
          <div><span class="label">Cached input</span><strong>{percentage(summary.cache.value)}</strong><small class="muted">{summary.cache.samples} recorded</small></div>
        </div>
        <p class="statuses">{summary.statuses.passed} passed · {summary.statuses.failed} failed · {summary.statuses.error} errors · {summary.statuses.running} running · {summary.statuses.interrupted} unfinished</p>
      </article>
    {/each}
  </div>
  <p class="footnote">Costs are estimated API equivalents in USD. Averages use recorded values; cached input is weighted by input tokens.</p>
</section>

<style>
  .heading { display: flex; align-items: baseline; justify-content: space-between; gap: 16px; margin-bottom: 7px; }
  .heading span { font-size: 12px; }
  .explanation, .footnote { font-size: 12px; color: var(--muted); }
  .explanation { max-width: 920px; margin-bottom: 16px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(255px, 1fr)); gap: 12px; }
  article { padding: 17px; border: 1px solid var(--border); border-radius: 7px; background: var(--panel); }
  article > .muted { font-size: 12px; }
  h3 { margin-bottom: 3px; }
  .measures { display: grid; grid-template-columns: 1fr 1fr; gap: 17px; margin: 20px 0 15px; }
  .measures > div { display: flex; flex-direction: column; }
  .label { color: var(--muted); font-size: 12px; }
  strong { font-weight: 550; font-size: 20px; font-variant-numeric: tabular-nums; }
  .statuses { color: var(--muted); font-size: 11px; padding-top: 12px; border-top: 1px solid var(--border); }
  .footnote { margin-top: 10px; }
</style>
