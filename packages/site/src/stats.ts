// The benchmark numbers on the page. They match the README charts drawn by
// assets/readme/charts.py; after a new run, update both.

export type Tool = "kraai" | "codex" | "claude";

export interface Headline {
  value: number;
  decimals: number;
  suffix: string;
}

export interface Bar {
  tool: Tool;
  value: number;
  label: string;
}

export interface Panel {
  headline: Headline;
  caption: string;
  bars: Bar[];
  /** The value a full-length bar stands for. Defaults to the largest bar. */
  maximum?: number;
}

export interface Figure {
  id: string;
  /** The one result the page leads with, and what it is a result of. */
  claim: { headline: Headline; label: string };
  title: string;
  tools: Partial<Record<Tool, string>>;
  panels: Panel[];
  /** The caveats, as trusted HTML written here. */
  note: string;
}

const tools: Tool[] = ["kraai", "codex", "claude"];

function bars(values: number[], labels: string[]): Bar[] {
  return values.map((value, index) => ({ tool: tools[index], value, label: labels[index] }));
}

const memory: Headline = { value: 6.7, decimals: 1, suffix: "×" };
const tokens: Headline = { value: 53, decimals: 0, suffix: "%" };
const cost: Headline = { value: 59, decimals: 0, suffix: "%" };

// Each multiple compares Kraai with the closest other agent.
export const resources: Figure = {
  id: "resources",
  claim: { headline: memory, label: "less memory than Codex or Claude Code" },
  title: "Each agent’s terminal UI, started fresh in the same repo and left idle.",
  tools: { kraai: "Kraai", codex: "Codex 0.167.0", claude: "Claude Code 2.1.322" },
  panels: [
    { headline: memory, caption: "less memory", bars: bars([40, 274, 270], ["40 MB", "274 MB", "270 MB"]) },
    {
      headline: { value: 14, decimals: 0, suffix: "×" },
      caption: "less CPU in the first 30 s",
      bars: bars([0.06, 0.87, 2.9], ["0.06 s", "0.87 s", "2.9 s"]),
    },
    {
      headline: { value: 11, decimals: 0, suffix: "×" },
      caption: "less CPU while idle",
      bars: bars([0.02, 0.8, 0.23], ["0.02%", "0.8%", "0.23%"]),
    },
  ],
  note:
    "Each number is the median of three runs on Linux in a 200×50 tmux pane, with idle CPU measured over " +
    "60 seconds. Only each agent’s own processes count, not the MCP servers it starts. Each multiple compares " +
    "Kraai with the closest other agent.",
};

export const codingTasks: Figure = {
  id: "coding-tasks",
  claim: { headline: tokens, label: "fewer tokens on three coding tasks" },
  title: "Three in-house coding tasks, both agents on gpt-5.6-sol at low reasoning effort. Both passed every hidden grader.",
  tools: { kraai: "Kraai", codex: "Codex CLI 0.159.0" },
  panels: [
    { headline: tokens, caption: "fewer tokens", bars: bars([159.6, 338.6], ["159.6k", "338.6k"]) },
    {
      headline: { value: 28, decimals: 0, suffix: "%" },
      caption: "lower estimated cost",
      bars: bars([0.47, 0.66], ["$0.47", "$0.66"]),
    },
  ],
  note:
    "Selected examples, not an average: each agent made one attempt per task. Kraai cost 6% more on one of " +
    "the three. Token totals include cached input, and costs are API-equivalent estimates.",
};

export const terminalBench: Figure = {
  id: "terminal-bench",
  claim: { headline: cost, label: "lower cost per trial on Terminal‑Bench 4" },
  title: "Twenty Terminal-Bench 4 tasks, both agents on gpt-6-astra at low reasoning effort.",
  tools: { kraai: "Kraai", codex: "Codex 0.151.0" },
  panels: [
    { headline: cost, caption: "lower cost per trial", bars: bars([1.2, 2.96], ["$1.20", "$2.96"]) },
    {
      headline: { value: 70, decimals: 0, suffix: "%" },
      caption: "fewer input tokens",
      bars: bars([486, 1630], ["486k", "1.63M"]),
    },
    {
      headline: { value: 90, decimals: 0, suffix: "%" },
      caption: "of tasks passed",
      bars: bars([90, 94], ["90%", "94%"]),
      // Drawn against 100% so the small gap in pass rate stays small.
      maximum: 100,
    },
  ],
  note:
    "These are the 20 tasks Codex finds easiest, so they say nothing about the hard half of the benchmark: " +
    "Codex passes 94% of them but only 51% of all 66. Pass rate is drawn against 100%. Kraai ran each task " +
    "twice and passed 36 of 40 trials; the Codex numbers are 94 of 100 trials from its " +
    '<a href="https://hub.harborframework.com/jobs/a538f69d-7756-444a-9dfe-43a0ab10b3a8">published leaderboard run</a>. ' +
    "Input tokens include cache reads. Cost is an estimate at API prices ($10/M input, $1/M cached input, " +
    "$50/M output).",
};

export const figures: Figure[] = [resources, codingTasks, terminalBench];
