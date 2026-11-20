"""Draws the README benchmark charts as light and dark SVGs.

The README charts read their numbers only from this file. After a new run,
edit them, update any README prose that quotes them, then run
`python3 assets/readme/charts.py` from the repository root.
"""

from dataclasses import dataclass
from html import escape
from pathlib import Path

OUTPUT = Path(__file__).parent

# One color per tool, kept the same in every chart. Validated as a categorical
# palette against GitHub's light (#ffffff) and dark (#0d1117) page colors.
THEMES = {
    "light": {
        "Kraai": "#2a78d6",
        "Codex": "#eb6834",
        "Claude Code": "#1baf7a",
        "text": "#1f2328",
        "muted": "#59636e",
        "rule": "#d1d9e0",
    },
    "dark": {
        "Kraai": "#3987e5",
        "Codex": "#d95926",
        "Claude Code": "#199e70",
        "text": "#f0f6fc",
        "muted": "#9198a1",
        "rule": "#3d444d",
    },
}


@dataclass
class Row:
    tool: str
    label: str


@dataclass
class Panel:
    title: str
    hint: str
    values: list[float]
    labels: list[str]
    maximum: float | None = None


@dataclass
class Figure:
    name: str
    title: str
    summary: str
    rows: list[Row]
    panels: list[Panel]


FIGURES = [
    Figure(
        name="terminal-bench",
        title="Terminal-Bench 4: 20 tasks, gpt-6-astra at low reasoning",
        summary="Kraai passes about as many tasks while reading 70% fewer input tokens, "
        "writing half the output, at 40% of the cost.",
        rows=[Row("Kraai", "Kraai"), Row("Codex", "Codex 0.151.0")],
        panels=[
            Panel("Tasks passed", "Higher is better", [36 / 40, 94 / 100], ["90%", "94%"], 1),
            Panel("Input tokens / trial", "Lower is better", [486, 1630], ["486k", "1.63M"]),
            Panel("Output tokens / trial", "Lower is better", [6.7, 13.2], ["6.7k", "13.2k"]),
            Panel("Cost / trial", "Lower is better", [1.20, 2.96], ["$1.20", "$2.96"]),
        ],
    ),
    Figure(
        name="resources",
        title="Idle terminal UI: memory and CPU",
        summary="Kraai's terminal UI uses about a seventh of the memory of Codex or Claude Code.",
        rows=[
            Row("Kraai", "Kraai"),
            Row("Codex", "Codex 0.167.0"),
            Row("Claude Code", "Claude Code 2.1.322"),
        ],
        panels=[
            Panel("Memory (RSS)", "Lower is better", [40, 274, 270], ["40 MB", "274 MB", "270 MB"]),
            Panel("CPU, first 30 s", "Lower is better", [0.06, 0.87, 2.9], ["0.06 s", "0.87 s", "2.9 s"]),
            Panel("CPU while idle", "Lower is better", [0.02, 0.8, 0.23], ["0.02%", "0.8%", "0.23%"]),
        ],
    ),
]

WIDTH = 900
PAD = 16
LABEL_COLUMN = 150
PANEL_GAP = 28
VALUE_ROOM = 52
BAR = 20
BAR_GAP = 12
RADIUS = 4
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'Noto Sans', Helvetica, Arial, sans-serif"


def bar_path(x: float, y: float, length: float) -> str:
    """A bar anchored square at the baseline with rounded data-end corners."""
    length = max(length, 2.0)
    r = min(RADIUS, length / 2, BAR / 2)
    return (
        f"M{x:.1f},{y:.1f}h{length - r:.1f}a{r},{r} 0 0 1 {r},{r}"
        f"v{BAR - 2 * r:.1f}a{r},{r} 0 0 1 {-r},{r}h{-(length - r):.1f}z"
    )


def text(x: float, y: float, body: str, size: int, fill: str, weight: int = 400, anchor: str = "start") -> str:
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" font-weight="{weight}" '
        f'fill="{fill}" text-anchor="{anchor}">{escape(body)}</text>'
    )


def render(figure: Figure, theme: dict[str, str]) -> str:
    columns = len(figure.panels)
    panel_width = (WIDTH - 2 * PAD - LABEL_COLUMN - (columns - 1) * PANEL_GAP) / columns
    bar_room = panel_width - VALUE_ROOM
    bars_top = 120
    bars_height = len(figure.rows) * BAR + (len(figure.rows) - 1) * BAR_GAP
    height = bars_top + bars_height + 20

    parts = [
        text(PAD, 28, figure.title, 16, theme["text"], 600),
        text(PAD, 50, figure.summary, 13, theme["muted"]),
    ]
    for index, row in enumerate(figure.rows):
        y = bars_top + index * (BAR + BAR_GAP) + BAR / 2 + 4.5
        weight = 600 if row.tool == "Kraai" else 400
        parts.append(text(PAD, y, row.label, 13, theme["text"], weight))

    for column, panel in enumerate(figure.panels):
        x = PAD + LABEL_COLUMN + column * (panel_width + PANEL_GAP)
        maximum = panel.maximum or max(panel.values)
        parts.append(text(x, 88, panel.title, 13, theme["text"], 600))
        parts.append(text(x, 104, panel.hint, 11, theme["muted"]))
        parts.append(
            f'<line x1="{x:.1f}" y1="{bars_top - 6}" x2="{x:.1f}" '
            f'y2="{bars_top + bars_height + 6}" stroke="{theme["rule"]}" stroke-width="1"/>'
        )
        for index, (row, value, label) in enumerate(zip(figure.rows, panel.values, panel.labels)):
            y = bars_top + index * (BAR + BAR_GAP)
            length = bar_room * value / maximum
            parts.append(f'<path d="{bar_path(x + 1, y, length)}" fill="{theme[row.tool]}"/>')
            parts.append(text(x + 1 + max(length, 2) + 6, y + BAR / 2 + 4.5, label, 12, theme["text"]))

    description = "; ".join(
        f"{panel.title}: "
        + ", ".join(f"{row.label} {label}" for row, label in zip(figure.rows, panel.labels))
        for panel in figure.panels
    )
    body = "\n  ".join(parts)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height:.0f}" '
        f'viewBox="0 0 {WIDTH} {height:.0f}" font-family="{FONT}" role="img" '
        f'aria-labelledby="title desc">\n'
        f"  <title id=\"title\">{escape(figure.title)}</title>\n"
        f"  <desc id=\"desc\">{escape(description)}</desc>\n"
        f'  <g style="font-variant-numeric: tabular-nums">\n  {body}\n  </g>\n</svg>\n'
    )


def main() -> None:
    for figure in FIGURES:
        for mode, theme in THEMES.items():
            path = OUTPUT / f"{figure.name}-{mode}.svg"
            path.write_text(render(figure, theme), encoding="utf-8")
            print(path)


if __name__ == "__main__":
    main()
