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
  title: string;
  tools: Partial<Record<Tool, string>>;
  panels: Panel[];
}

const tools: Tool[] = ["kraai", "codex", "claude"];

function bars(values: number[], labels: string[]): Bar[] {
  return values.map((value, index) => ({ tool: tools[index], value, label: labels[index] }));
}

// Each multiple compares Kraai with the closest other agent.
export const resources: Figure = {
  title: "Idle terminal UI memory and CPU for Kraai, Codex and Claude Code",
  tools: { kraai: "Kraai", codex: "Codex 0.167.0", claude: "Claude Code 2.1.322" },
  panels: [
    {
      headline: { value: 6.7, decimals: 1, suffix: "×" },
      caption: "less memory",
      bars: bars([40, 274, 270], ["40 MB", "274 MB", "270 MB"]),
    },
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
};

export const codingTasks: Figure = {
  title: "Kraai and Codex CLI on three coding tasks both passed",
  tools: { kraai: "Kraai", codex: "Codex CLI 0.159.0" },
  panels: [
    {
      headline: { value: 53, decimals: 0, suffix: "%" },
      caption: "fewer tokens",
      bars: bars([159.6, 338.6], ["159.6k", "338.6k"]),
    },
    {
      headline: { value: 28, decimals: 0, suffix: "%" },
      caption: "lower estimated cost",
      bars: bars([0.47, 0.66], ["$0.47", "$0.66"]),
    },
  ],
};

export const terminalBench: Figure = {
  title: "Kraai and Codex on 20 Terminal-Bench 4 tasks",
  tools: { kraai: "Kraai", codex: "Codex 0.151.0" },
  panels: [
    {
      headline: { value: 59, decimals: 0, suffix: "%" },
      caption: "lower cost per trial",
      bars: bars([1.2, 2.96], ["$1.20", "$2.96"]),
    },
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
};
