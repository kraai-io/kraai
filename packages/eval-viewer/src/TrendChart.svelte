<script lang="ts">
  import { date, metric as formatMetric, percentage, versionLabel } from "./format.ts";
  import type { HistoryMetric, HistoryPoint } from "./history.ts";

  let { points, reference, metric, label, taskCount }: {
    points: HistoryPoint[];
    reference?: HistoryPoint;
    metric: HistoryMetric;
    label: string;
    taskCount: number;
  } = $props();
  let selectedId = $state("");

  const measured = $derived(points.filter((point) => point.value !== null));
  const selected = $derived(measured.find((point) => point.version.id === selectedId) ?? measured.at(-1));
  const minimumTime = $derived(Math.min(...points.map((point) => point.version.latest_at_ms!)));
  const maximumTime = $derived(Math.max(...points.map((point) => point.version.latest_at_ms!)));
  const maximumValue = $derived.by(() => {
    if (metric === "pass_rate" || metric === "cache_rate") return 1;
    const maximum = Math.max(...measured.map((point) => point.value!), reference?.value ?? 0);
    if (maximum <= 0) return 1;
    const interval = maximum * 1.05 / 4;
    const magnitude = 10 ** Math.floor(Math.log10(interval));
    const step = [1, 2, 2.5, 5, 10].find((value) => value >= interval / magnitude)!;
    return step * magnitude * 4;
  });
  const ticks = $derived([1, 0.75, 0.5, 0.25, 0].map((fraction) => ({ fraction, value: maximumValue * fraction })));
  const segments = $derived.by(() => {
    const values: string[] = [];
    let segment: string[] = [];
    for (const point of points) {
      if (point.value === null) {
        if (segment.length) values.push(segment.join(" "));
        segment = [];
      } else {
        segment.push(`${x(point) * 10},${y(point.value) * 2.6}`);
      }
    }
    if (segment.length) values.push(segment.join(" "));
    return values;
  });

  function x(point: HistoryPoint): number {
    return minimumTime === maximumTime ? 50 : 3 + ((point.version.latest_at_ms! - minimumTime) / (maximumTime - minimumTime)) * 94;
  }

  function y(value: number): number {
    return (1 - value / maximumValue) * 100;
  }

  function format(value: number | null): string {
    return metric === "pass_rate" || metric === "cache_rate" ? percentage(value) : formatMetric(value, metric);
  }

  function shortDate(value: number): string {
    return new Date(value).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }
</script>

{#if measured.length === 0}
  <p class="empty">No dated Kraai version has {label.toLowerCase()} recorded for every selected task.</p>
{:else}
  <div class="chart">
    <div class="axis" aria-hidden="true">{#each ticks as tick}<span style:top={`${(1 - tick.fraction) * 100}%`}>{format(tick.value)}</span>{/each}</div>
    <div class="plot">
      <svg viewBox="0 0 1000 260" preserveAspectRatio="none" role="img" aria-label={`${label} by latest run date. Select a point for its values.`}>
        {#each ticks as tick}<line class="grid" x1="0" x2="1000" y1={(1 - tick.fraction) * 260} y2={(1 - tick.fraction) * 260} />{/each}
        {#if reference?.value !== null && reference?.value !== undefined}<line class="reference" x1="0" x2="1000" y1={y(reference.value) * 2.6} y2={y(reference.value) * 2.6} />{/if}
        {#each segments as segment}<polyline class="trend" points={segment} />{/each}
      </svg>
      {#each measured as point (point.version.id)}
        <button
          class="point"
          class:active={selected?.version.id === point.version.id}
          style:left={`${x(point)}%`}
          style:top={`${y(point.value!)}%`}
          aria-label={`${versionLabel(point.version)}: ${format(point.value)}, ${point.samples} samples across ${point.tasks} tasks`}
          aria-pressed={selected?.version.id === point.version.id}
          onpointerenter={() => selectedId = point.version.id}
          onfocus={() => selectedId = point.version.id}
          onclick={() => selectedId = point.version.id}
        ><span></span></button>
      {/each}
    </div>
    <div class="time-axis" aria-hidden="true">
      {#if minimumTime === maximumTime}<span>{shortDate(minimumTime)}</span>{:else}<span>{shortDate(minimumTime)}</span><span>{shortDate(maximumTime)}</span>{/if}
    </div>
  </div>
  <div class="readout" aria-live="polite" aria-atomic="true">
    {#if selected}
      <div><strong>{format(selected.value)}</strong><span title={selected.version.label}>{versionLabel(selected.version)}</span></div>
      <p>{date(selected.version.latest_at_ms)} · {selected.samples} {selected.samples === 1 ? "sample" : "samples"} from {selected.attempts} finished {selected.attempts === 1 ? "attempt" : "attempts"} · {selected.tasks}/{taskCount} tasks</p>
    {/if}
  </div>
  {#if measured.length === 1}<p class="hint">One measured version. More versions will show the trend.</p>{/if}
  {#if measured.length < points.length}<p class="hint">{points.length - measured.length} dated {points.length - measured.length === 1 ? "version lacks" : "versions lack"} results for this selection.</p>{/if}
{/if}

<style>
  .chart { display: grid; grid-template-columns: 78px minmax(0, 1fr); grid-template-rows: 260px auto; gap: 12px; padding: 12px 10px 0 0; }
  .axis { position: relative; color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
  .axis span { position: absolute; right: 0; max-width: 100%; transform: translateY(-50%); text-align: right; }
  .plot { position: relative; min-width: 0; }
  svg { width: 100%; height: 100%; display: block; overflow: visible; }
  line, polyline { vector-effect: non-scaling-stroke; }
  .grid { stroke: var(--border); stroke-width: 1; }
  .reference { stroke: #7fd0b3; stroke-width: 1.5; stroke-dasharray: 5 5; }
  .trend { fill: none; stroke: var(--accent); stroke-width: 2; }
  .point { position: absolute; width: 26px; height: 26px; min-height: 0; padding: 0; border: 0; background: transparent; display: grid; place-items: center; transform: translate(-50%, -50%); border-radius: 50%; }
  .point span { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); border: 2px solid #0f1116; box-sizing: content-box; }
  .point:hover, .point.active { background: #b0b8ff20; }
  .point.active span { background: #e0e3ff; }
  .time-axis { grid-column: 2; display: flex; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 11px; padding: 0 3%; }
  .time-axis span:only-child { margin: 0 auto; }
  .readout { min-height: 76px; margin-top: 18px; padding: 13px 16px; background: var(--panel); border: 1px solid var(--border); border-radius: 6px; font-size: 12px; }
  .readout > div { display: flex; align-items: baseline; gap: 13px; overflow-wrap: anywhere; }
  .readout strong { color: var(--accent); font-size: 18px; font-weight: 550; font-variant-numeric: tabular-nums; white-space: nowrap; }
  .readout p { color: var(--muted); margin-top: 3px; }
  .hint { color: var(--muted); font-size: 12px; margin-top: 10px; }
  @media (max-width: 600px) { .chart { grid-template-columns: 68px minmax(0, 1fr); gap: 8px; } .time-axis { font-size: 10px; } .readout > div { flex-wrap: wrap; gap: 4px 12px; } }
</style>
