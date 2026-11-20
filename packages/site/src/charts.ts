// Draws a benchmark figure as HTML in the style of the README charts: one large
// headline number per panel, a short caption, and a bar per agent.
import { barFraction, headlineText } from "./format.ts";
import { toolLogo } from "./logos.ts";
import type { Figure } from "./stats.ts";

function escape(text: string): string {
  return text.replace(/[&<>"]/g, (char) => `&#${char.charCodeAt(0)};`);
}

/** The figure in words, for screen readers; the drawn panels are hidden from them. */
export function describe(figure: Figure): string {
  return figure.panels
    .map((panel) => {
      const bars = panel.bars.map((bar) => `${figure.tools[bar.tool]} ${bar.label}`).join(", ");
      return `${headlineText(panel.headline)} ${panel.caption}: ${bars}.`;
    })
    .join(" ");
}

export function renderFigure(figure: Figure): string {
  let order = 0;
  const panels = figure.panels.map((panel) => {
    const { value, decimals, suffix } = panel.headline;
    const rows = panel.bars.map((bar) => {
      const kraai = bar.tool === "kraai";
      const fraction = barFraction(panel, bar).toFixed(4);
      return `
        <li class="bar-row${kraai ? " is-kraai" : ""}" style="--fraction: ${fraction}; --order: ${order++}">
          <span class="tool">${toolLogo(bar.tool)}<span>${escape(figure.tools[bar.tool] ?? bar.tool)}</span></span>
          <span class="track">
            <span class="bar">
              ${kraai ? '<span class="bar-glow"></span>' : ""}
              <span class="bar-clip"><span class="bar-fill"></span></span>
            </span>
            <span class="bar-value">${escape(bar.label)}</span>
          </span>
        </li>`;
    });
    return `
      <div class="panel">
        <p class="headline" data-count="${value}" data-decimals="${decimals}" data-suffix="${escape(suffix)}">${escape(headlineText(panel.headline))}</p>
        <p class="caption">${escape(panel.caption)}</p>
        <ul class="bars">${rows.join("")}</ul>
      </div>`;
  });
  return `
    <p class="visually-hidden">${escape(describe(figure))}</p>
    <div class="panels" style="--panels: ${figure.panels.length}" aria-hidden="true">${panels.join("")}</div>`;
}
