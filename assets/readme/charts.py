"""Draws the README benchmark charts as light and dark SVGs.

The coding-task numbers come from docs/benchmark-homepage-examples.json; the
others live only in this file. After a new run, edit them, update any README
prose that quotes them, then run `python3 assets/readme/charts.py` from the
repository root.

Each chart is a row of panels: one large headline number, a short caption, and
a bar per agent. Kraai's bar and number take the gradient; the others stay grey.
"""

import json
from dataclasses import dataclass
from html import escape
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUTPUT = Path(__file__).parent
CODING_DATA = ROOT / "docs" / "benchmark-homepage-examples.json"

THEMES = {
    "dark": {
        "background": "#0a0a0c",
        "border": "#1f1f24",
        "halo": "#7b6cff",
        "halo_opacity": 0.22,
        "text": "#f5f5f7",
        "muted": "#8e8e96",
        "other": "#3d3d45",
        "gradient": ["#5b6cff", "#9a7bff", "#e38cff"],
        "glow": 0.65,
    },
    "light": {
        "background": "#fbfbfd",
        "border": "#e5e5ea",
        "halo": "#7b6cff",
        "halo_opacity": 0.10,
        "text": "#1d1d1f",
        "muted": "#6e6e73",
        "other": "#d2d2d7",
        "gradient": ["#3f4fe6", "#7a52f0", "#c246d6"],
        "glow": 0.35,
    },
}

WIDTH = 900
PAD = 44
GAP = 44
FONT = "'SF Pro Display', -apple-system, BlinkMacSystemFont, Inter, 'Segoe UI', sans-serif"
BAR = 20
BAR_STEP = 50
BARS_TOP = 168
VALUE_ROOM = 76


@dataclass
class Bar:
    tool: str
    value: float
    label: str


@dataclass
class Panel:
    headline: str
    caption: str
    bars: list[Bar]
    maximum: float | None = None


@dataclass
class Figure:
    name: str
    title: str
    panels: list[Panel]
    tool_names: dict[str, str]


def text(x, y, body, size, fill, weight=400, spacing=0.0):
    letter = f' letter-spacing="{spacing}"' if spacing else ""
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" font-weight="{weight}" '
        f'fill="{fill}"{letter}>{escape(body)}</text>'
    )


def pill_bar(x, y, length):
    """A bar rounded at both ends, so short bars still read as bars."""
    length = max(length, BAR)
    r = BAR / 2
    return f'x="{x:.1f}" y="{y:.1f}" width="{length:.1f}" height="{BAR}" rx="{r}"'


def defs(theme: dict) -> str:
    stops = theme["gradient"]
    spread = "".join(
        f'<stop offset="{i / (len(stops) - 1):.2f}" stop-color="{color}"/>' for i, color in enumerate(stops)
    )
    return (
        "<defs>"
        f'<linearGradient id="kraai" x1="0" y1="0" x2="1" y2="0">{spread}</linearGradient>'
        f'<linearGradient id="kraai-text" x1="0" y1="0" x2="1" y2="1">{spread}</linearGradient>'
        '<radialGradient id="halo" cx="0.5" cy="0" r="0.8">'
        f'<stop offset="0" stop-color="{theme["halo"]}" stop-opacity="{theme["halo_opacity"]}"/>'
        f'<stop offset="1" stop-color="{theme["halo"]}" stop-opacity="0"/>'
        "</radialGradient>"
        '<filter id="glow" x="-30%" y="-150%" width="160%" height="400%">'
        '<feGaussianBlur stdDeviation="9"/>'
        "</filter>"
        "</defs>"
    )


def render(figure: Figure, theme: dict) -> str:
    count = len(figure.panels)
    panel_width = (WIDTH - 2 * PAD - GAP * (count - 1)) / count
    bar_room = panel_width - VALUE_ROOM
    bar_count = max(len(panel.bars) for panel in figure.panels)
    height = BARS_TOP + bar_count * BAR_STEP + 14

    parts = [
        f'<rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="20" '
        f'fill="{theme["background"]}" stroke="{theme["border"]}"/>',
        f'<rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="20" fill="url(#halo)"/>',
    ]
    for index, panel in enumerate(figure.panels):
        x = PAD + index * (panel_width + GAP)
        parts.append(text(x, 104, panel.headline, 68, "url(#kraai-text)", 700, -2.5))
        parts.append(text(x, 134, panel.caption, 15, theme["text"], 500, -0.2))
        maximum = panel.maximum or max(bar.value for bar in panel.bars)
        for b, bar in enumerate(panel.bars):
            y = BARS_TOP + b * BAR_STEP
            kraai = bar.tool == "kraai"
            name_color = theme["text"] if kraai else theme["muted"]
            parts.append(text(x, y + 2, figure.tool_names[bar.tool], 12, name_color, 600 if kraai else 400))
            length = max(bar_room * bar.value / maximum, BAR)
            shape = pill_bar(x, y + 10, length)
            if kraai:
                parts.append(f'<rect {shape} fill="url(#kraai)" opacity="{theme["glow"]}" filter="url(#glow)"/>')
                parts.append(f'<rect {shape} fill="url(#kraai)"/>')
            else:
                parts.append(f'<rect {shape} fill="{theme["other"]}"/>')
            parts.append(
                text(x + length + 10, y + 25, bar.label, 14, name_color, 600 if kraai else 400, -0.2)
            )

    description = " ".join(
        f"{panel.headline} {panel.caption}: "
        + ", ".join(f"{figure.tool_names[bar.tool]} {bar.label}" for bar in panel.bars)
        + "."
        for panel in figure.panels
    )
    body = "\n  ".join(parts)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'viewBox="0 0 {WIDTH} {height}" font-family="{FONT}" role="img" aria-labelledby="title desc">\n'
        f'  <title id="title">{escape(figure.title)}</title>\n'
        f'  <desc id="desc">{escape(description)}</desc>\n'
        f"  {defs(theme)}\n"
        f'  <g style="font-variant-numeric: tabular-nums">\n  {body}\n  </g>\n</svg>\n'
    )


def thousands(value: float) -> str:
    return f"{value / 1000:.1f}k"


def coding_tasks() -> Figure:
    data = json.loads(CODING_DATA.read_text(encoding="utf-8"))
    total = data["aggregation"]
    kraai_tokens = sum(task["kraai"]["total_tokens"] for task in data["tasks"].values())
    codex_tokens = sum(task["codex"]["total_tokens"] for task in data["tasks"].values())
    kraai_cost = sum(task["kraai"]["estimated_api_equivalent_usd"] for task in data["tasks"].values())
    codex_cost = sum(task["codex"]["estimated_api_equivalent_usd"] for task in data["tasks"].values())
    return Figure(
        name="coding-tasks",
        title="Kraai and Codex CLI on three coding tasks both passed",
        panels=[
            Panel(f"{total['token_reduction_pct']:.0f}%", "fewer tokens", [
                Bar("kraai", kraai_tokens, thousands(kraai_tokens)),
                Bar("codex", codex_tokens, thousands(codex_tokens)),
            ]),
            Panel(f"{total['cost_reduction_pct']:.0f}%", "lower estimated cost", [
                Bar("kraai", kraai_cost, f"${kraai_cost:.2f}"),
                Bar("codex", codex_cost, f"${codex_cost:.2f}"),
            ]),
        ],
        tool_names={"kraai": "Kraai", "codex": "Codex CLI 0.159.0"},
    )


def terminal_bench() -> Figure:
    return Figure(
        name="terminal-bench",
        title="Kraai and Codex on 20 Terminal-Bench 4 tasks",
        panels=[
            Panel("59%", "lower cost per trial", [Bar("kraai", 1.20, "$1.20"), Bar("codex", 2.96, "$2.96")]),
            Panel("70%", "fewer input tokens", [Bar("kraai", 486, "486k"), Bar("codex", 1630, "1.63M")]),
            # Drawn against 100% so the small gap in pass rate stays small.
            Panel("90%", "of tasks passed", [Bar("kraai", 90, "90%"), Bar("codex", 94, "94%")], maximum=100),
        ],
        tool_names={"kraai": "Kraai", "codex": "Codex 0.151.0"},
    )


def resources() -> Figure:
    tools = ["kraai", "codex", "claude"]

    def bars(values, labels):
        return [Bar(tool, value, label) for tool, value, label in zip(tools, values, labels)]

    # Each multiple compares Kraai with the closest other agent.
    return Figure(
        name="resources",
        title="Idle terminal UI memory and CPU for Kraai, Codex and Claude Code",
        panels=[
            Panel("6.7×", "less memory", bars([40, 274, 270], ["40 MB", "274 MB", "270 MB"])),
            Panel("14×", "less CPU at startup", bars([0.06, 0.87, 2.9], ["0.06 s", "0.87 s", "2.9 s"])),
            Panel("11×", "less CPU at idle", bars([0.02, 0.8, 0.23], ["0.02%", "0.8%", "0.23%"])),
        ],
        tool_names={"kraai": "Kraai", "codex": "Codex 0.167.0", "claude": "Claude Code 2.1.322"},
    )


def main() -> None:
    for figure in (resources(), coding_tasks(), terminal_bench()):
        for mode, theme in THEMES.items():
            path = OUTPUT / f"{figure.name}-{mode}.svg"
            path.write_text(render(figure, theme), encoding="utf-8")
            print(path.relative_to(ROOT))


if __name__ == "__main__":
    main()
