<script lang="ts">
  import type { Version } from "./types.ts";
  import type { TaskGroup } from "./results.ts";
  import { metric, passFraction, percentage, versionLabel } from "./format.ts";
  import TaskDetails from "./TaskDetails.svelte";

  let { tasks, versions }: { tasks: TaskGroup[]; versions: Version[] } = $props();
  let search = $state("");
  let expanded = $state<string[]>([]);
  const visible = $derived(tasks.filter((task) => task.name.toLowerCase().includes(search.toLowerCase())));

  function toggle(name: string) {
    expanded = expanded.includes(name) ? expanded.filter((item) => item !== name) : [...expanded, name];
  }
</script>

<section aria-label="Task results">
  <div class="heading"><h2>Tasks <span class="muted">{visible.length}</span></h2><input type="search" aria-label="Search tasks" placeholder="Search tasks…" bind:value={search} /></div>
  {#if visible.length === 0}
    <p class="empty">{tasks.length ? "No tasks match your search." : "No attempts for these versions."}</p>
  {:else}
    <div class="scroll table-wrap">
      <table>
        <thead><tr><th>Task</th><th>Version</th><th>Pass fraction</th><th>Cost / attempt</th><th>Runtime / attempt</th><th>Cached input</th></tr></thead>
        {#each visible as task (task.name)}
          <tbody>
            {#each task.summaries as summary, index}
              <tr>
                {#if index === 0}
                  <td rowspan={versions.length} class="task-name">
                    <button aria-expanded={expanded.includes(task.name)} onclick={() => toggle(task.name)}><span aria-hidden="true">{expanded.includes(task.name) ? "▾" : "▸"}</span> {task.name}</button>
                    {#if !task.comparable}<span class="sample">Not in shared comparison</span>{/if}
                  </td>
                {/if}
                <td class="version-label" title={versions[index].label}>{versionLabel(versions[index])}</td>
                <td class="number">{passFraction(summary)}<span class="sample">{summary.statuses.error} errors · {summary.statuses.running} running · {summary.statuses.interrupted} unfinished</span></td>
                <td class="number">{metric(summary.metrics.cost_usd.value, "cost_usd")}</td>
                <td class="number">{metric(summary.metrics.duration_ms.value, "duration_ms")}</td>
                <td class="number">{percentage(summary.cache.value)}</td>
              </tr>
            {/each}
            {#if expanded.includes(task.name)}<tr><td colspan="6" class="expanded"><TaskDetails {task} {versions} /></td></tr>{/if}
          </tbody>
        {/each}
      </table>
    </div>
  {/if}
</section>

<style>
  .heading { display: flex; justify-content: space-between; align-items: center; gap: 16px; margin-bottom: 14px; }
  h2 span { font-weight: 400; font-size: 13px; padding-left: 5px; }
  input { width: 260px; max-width: 65%; }
  .table-wrap { border: 1px solid var(--border); border-radius: 7px; }
  thead { background: var(--panel); }
  th { white-space: nowrap; }
  tbody { border-top: 1px solid var(--border); }
  tbody tr:last-child > td { border-bottom: 0; }
  .task-name { width: 25%; min-width: 200px; vertical-align: top; }
  .task-name button { background: none; border: 0; padding: 0; text-align: left; color: #dfe1ee; font-weight: 550; overflow-wrap: anywhere; }
  .task-name button:hover { color: var(--accent); }
  .task-name button span { color: var(--muted); display: inline-block; width: 13px; }
  .task-name > .sample { margin-left: 18px; }
  .version-label { min-width: 150px; max-width: 220px; }
  .expanded { padding: 0; }
</style>
