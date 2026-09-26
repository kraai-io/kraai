<script lang="ts">
  import { onMount } from "svelte";
  import type { Catalog } from "./types.ts";
  import { compareTasks, defaultSelection, preserveSelection } from "./results.ts";
  import VersionPicker from "./VersionPicker.svelte";
  import Overview from "./Overview.svelte";
  import TaskTable from "./TaskTable.svelte";
  import History from "./History.svelte";

  let catalog = $state<Catalog>({ versions: [], attempts: [], warnings: [] });
  let benchmark = $state("");
  let model = $state("");
  let selected = $state<string[]>(["", ""]);
  let view = $state<"comparison" | "history">("comparison");
  let loaded = $state(false);
  let refreshing = $state(false);
  let error = $state("");
  let updatedAt = $state("");
  let request: AbortController | undefined;
  let disposed = false;

  const benchmarks = $derived([...new Set(catalog.versions.map((version) => version.benchmark))].sort());
  const models = $derived([...new Set(catalog.versions.filter((version) => version.benchmark === benchmark).map((version) => JSON.stringify(version.model)))].sort());
  const available = $derived(catalog.versions.filter((version) => version.benchmark === benchmark && JSON.stringify(version.model) === model).sort((a, b) => (b.latest_at_ms ?? 0) - (a.latest_at_ms ?? 0)));
  const versions = $derived(selected.flatMap((id) => available.filter((version) => version.id === id)));
  const comparison = $derived(compareTasks(catalog.attempts, versions));

  function chooseBenchmark(value: string) {
    benchmark = value;
    const matching = catalog.versions.filter((version) => version.benchmark === value);
    if (!matching.some((version) => JSON.stringify(version.model) === model)) model = JSON.stringify(matching[0]?.model ?? null);
    selected = defaultSelection(matching.filter((version) => JSON.stringify(version.model) === model));
  }

  function chooseModel(value: string) {
    model = value;
    selected = defaultSelection(catalog.versions.filter((version) => version.benchmark === benchmark && JSON.stringify(version.model) === value));
  }

  async function refresh() {
    if (refreshing || disposed) return;
    refreshing = true;
    const controller = new AbortController();
    request = controller;
    const timeout = setTimeout(() => controller.abort(), 15000);
    try {
      const response = await fetch("/api/catalog", { signal: controller.signal, cache: "no-store" });
      if (!response.ok) throw new Error(`Could not load results (${response.status}).`);
      const next = await response.json() as Catalog;
      if (disposed) return;
      const nextBenchmark = next.versions.some((version) => version.benchmark === benchmark) ? benchmark : next.versions[0]?.benchmark ?? "";
      const matching = next.versions.filter((version) => version.benchmark === nextBenchmark);
      const nextModel = matching.some((version) => JSON.stringify(version.model) === model) ? model : JSON.stringify(matching[0]?.model ?? null);
      const choices = matching.filter((version) => JSON.stringify(version.model) === nextModel);
      selected = loaded && benchmark === nextBenchmark && model === nextModel
        ? preserveSelection(selected, choices)
        : defaultSelection(choices);
      benchmark = nextBenchmark;
      model = nextModel;
      catalog = next;
      loaded = true;
      error = "";
      updatedAt = new Date().toLocaleTimeString();
    } catch (failure) {
      if (!disposed) error = controller.signal.aborted ? "Results took too long to load." : failure instanceof Error ? failure.message : "Could not load results.";
    } finally {
      clearTimeout(timeout);
      if (!disposed) refreshing = false;
    }
  }

  onMount(() => {
    void refresh();
    const interval = setInterval(() => void refresh(), 5000);
    return () => {
      disposed = true;
      clearInterval(interval);
      request?.abort();
    };
  });
</script>

<main>
  <header>
    <div><p class="eyebrow">KRAAI</p><h1>Benchmark results</h1></div>
    <div class="refresh"><span class="muted">{updatedAt ? `Updated ${updatedAt}` : "Loading results"}</span><button onclick={() => void refresh()} disabled={refreshing}>{refreshing ? "Refreshing…" : "Refresh"}</button></div>
  </header>

  {#if error}<p class="notice" role="alert">{error} {loaded ? "Showing the last loaded results. " : ""}Retrying automatically.</p>{/if}
  {#if catalog.warnings.length}
    <details class="notice"><summary>{catalog.warnings.length} result {catalog.warnings.length === 1 ? "warning" : "warnings"}</summary><ul>{#each catalog.warnings as warning}<li>{warning}</li>{/each}</ul></details>
  {/if}

  {#if !loaded}
    <p class="empty" role="status">{error ? "Results are unavailable. Check that the viewer is running." : "Loading benchmark results…"}</p>
  {:else if catalog.versions.length === 0}
    <div class="empty"><h2>No results yet</h2><p>Results will appear here when a benchmark produces attempts.</p></div>
  {:else}
    <section class="filters" aria-label="Result filters">
      <div class="scope">
        <label>Benchmark<select aria-label="Benchmark" value={benchmark} onchange={(event) => chooseBenchmark(event.currentTarget.value)}>{#each benchmarks as value}<option value={value}>{value}</option>{/each}</select></label>
        <label>Model<select aria-label="Model" value={model} onchange={(event) => chooseModel(event.currentTarget.value)}>{#each models as value}<option value={value}>{JSON.parse(value) ?? "Unknown model"}</option>{/each}</select></label>
      </div>
      <VersionPicker versions={available} attempts={catalog.attempts} {selected} onchange={(value) => selected = value} />
    </section>
    <nav class="views" aria-label="Results view">
      <button aria-pressed={view === "comparison"} onclick={() => view = "comparison"}>Comparison</button>
      <button aria-pressed={view === "history"} onclick={() => view = "history"}>History</button>
    </nav>
    {#if view === "history"}
      {#key `${benchmark}:${model}`}
        <History attempts={catalog.attempts} versions={available} selected={versions} />
      {/key}
    {:else if versions.length === 0}
      <p class="empty">Select a version to see its results.</p>
    {:else}
      <Overview {versions} summaries={comparison.summaries} shared={comparison.shared} total={comparison.tasks.length} />
      <TaskTable {versions} tasks={comparison.tasks} />
    {/if}
  {/if}
</main>

<style>
  main { width: min(1460px, 100%); margin: 0 auto; padding: 32px 36px 64px; display: flex; flex-direction: column; gap: 26px; }
  header { display: flex; justify-content: space-between; align-items: center; gap: 20px; }
  .eyebrow { color: var(--accent); font-size: 10px; font-weight: 650; letter-spacing: 2px; margin-bottom: 5px; }
  .refresh { display: flex; gap: 12px; align-items: center; }
  .refresh span { font-size: 12px; }
  .filters { display: flex; flex-direction: column; gap: 18px; border-top: 1px solid var(--border); border-bottom: 1px solid var(--border); padding: 21px 0; }
  .scope { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 2fr); gap: 14px; }
  .views { display: flex; gap: 6px; }
  .views button { background: transparent; border-color: transparent; color: var(--muted); }
  .views button[aria-pressed="true"] { background: var(--panel); border-color: var(--border); color: #e4e6ed; }
  label { display: flex; flex-direction: column; gap: 6px; color: var(--muted); font-size: 12px; }
  select { color: #e4e6ed; font-size: 14px; width: 100%; }
  .empty p { margin-top: 8px; }
  summary { cursor: pointer; }
  li { overflow-wrap: anywhere; }
  @media (max-width: 700px) { main { padding: 22px 16px 48px; } header { align-items: start; } .refresh { flex-direction: column-reverse; align-items: end; gap: 5px; } .refresh span { font-size: 10px; } .scope { grid-template-columns: 1fr; } }
</style>
