<script lang="ts">
  import { metricKeys } from "./types.ts";
  import type { Attempt, AttemptLog } from "./types.ts";
  import { date, metric, metricLabels } from "./format.ts";

  let { attempt }: { attempt: Attempt } = $props();
  let selectedLog = $state("");
  let log = $state<AttemptLog | null>(null);
  let loading = $state(false);
  let error = $state("");
  let reload = $state(0);
  const attemptId = $derived(attempt.id);

  $effect(() => {
    const name = selectedLog;
    const id = attemptId;
    reload;
    log = null;
    error = "";
    loading = false;
    if (!name) return;
    const controller = new AbortController();
    loading = true;
    fetch(`/api/attempts/${encodeURIComponent(id)}/logs/${encodeURIComponent(name)}`, { signal: controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error(`Could not read log (${response.status}).`);
        const result = await response.json() as AttemptLog;
        if (!controller.signal.aborted) log = result;
      })
      .catch((failure: unknown) => {
        if (!controller.signal.aborted) error = failure instanceof Error ? failure.message : "Could not read log.";
      })
      .finally(() => { if (!controller.signal.aborted) loading = false; });
    return () => controller.abort();
  });
</script>

<section aria-label={`Attempt ${attempt.attempt} details`}>
  <div class="heading"><h3>Attempt {attempt.attempt}</h3><span class="muted">{date(attempt.started_at_ms)}</span></div>
  {#if attempt.error}<p class="notice">{attempt.error}</p>{/if}
  <dl>
    {#each metricKeys as key}
      <div><dt>{metricLabels[key]}</dt><dd>{metric(attempt.metrics[key], key)}</dd></div>
    {/each}
  </dl>
  <div class="log-heading">
    <label>Log
      <select aria-label="Log" bind:value={selectedLog}>
        <option value="">Choose a log</option>
        {#each attempt.logs as name}<option value={name}>{name}</option>{/each}
      </select>
    </label>
    {#if selectedLog}<button onclick={() => reload++} disabled={loading}>Reload log</button>{/if}
  </div>
  {#if attempt.logs.length === 0}<p class="muted">No logs recorded.</p>{/if}
  {#if loading}<p class="muted" role="status">Loading log…</p>{/if}
  {#if error}<p class="notice" role="alert">{error}</p>{/if}
  {#if log}
    {#if log.truncated}<p class="notice">This log is truncated.</p>{/if}
    <textarea readonly rows="18" wrap="off" aria-label={log.name} value={log.content || "Empty log."}></textarea>
  {/if}
</section>

<style>
  section { padding: 18px; border-top: 1px solid var(--border); background: #101218; }
  .heading { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 10px; margin-bottom: 14px; }
  .heading span { font-size: 12px; }
  dl { display: grid; grid-template-columns: repeat(auto-fit, minmax(130px, 1fr)); gap: 14px; margin: 16px 0 22px; }
  dt { color: var(--muted); font-size: 11px; }
  dd { margin: 3px 0 0; font-variant-numeric: tabular-nums; }
  .log-heading { display: flex; align-items: end; gap: 10px; margin-bottom: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; min-width: 0; color: var(--muted); font-size: 12px; }
  select { width: 340px; color: #e4e6ed; }
  button { flex-shrink: 0; }
  textarea { display: block; width: 100%; margin: 12px 0 0; padding: 16px; min-height: 240px; max-height: 720px; overflow: auto; resize: vertical; background: #0a0c10; color: #e4e6ed; border: 1px solid var(--border); border-radius: 5px; font: 12px/1.65 ui-monospace, SFMono-Regular, Consolas, monospace; tab-size: 2; }
</style>
