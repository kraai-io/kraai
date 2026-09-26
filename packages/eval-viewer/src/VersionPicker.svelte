<script lang="ts">
  import type { Attempt, Version } from "./types.ts";
  import { versionLabel, versionStats } from "./format.ts";
  import { isCodex, summarizeVersions } from "./results.ts";

  let { attempts, versions, selected, onchange }: {
    attempts: Attempt[];
    versions: Version[];
    selected: string[];
    onchange: (selected: string[]) => void;
  } = $props();
  const slots = ["Kraai", "Codex"];
  const summaries = $derived(summarizeVersions(attempts, versions));

  function choose(index: number, value: string) {
    onchange(selected.map((id, position) => position === index ? value : id));
  }
</script>

<div class="versions">
  {#each slots as slot, index}
    <label>
      <span>{slot}</span>
      <select aria-label={`${slot} version`} value={selected[index] ?? ""} onchange={(event) => choose(index, event.currentTarget.value)}>
        <option value="">None</option>
        {#each versions.filter((version) => isCodex(version) === (index === 1)) as version (version.id)}
          {@const stats = versionStats(summaries.get(version.id)!)}
          <option value={version.id} title={`${version.label} · ${stats}`} disabled={selected.some((id, position) => id === version.id && position !== index)}>
            {versionLabel(version)} · {stats}
          </option>
        {/each}
      </select>
    </label>
  {/each}
</div>

<style>
  .versions { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  span { font-size: 12px; color: var(--muted); }
  select { width: 100%; }
  @media (max-width: 620px) { .versions { grid-template-columns: 1fr; } }
</style>
