// The numbers section: three headline results that act as tabs, each opening
// the full chart behind it in the style of the README charts.
import { barFraction, headlineText } from "./format.ts";
import { toolLogo } from "./logos.ts";
import type { Figure, Headline } from "./stats.ts";

function escape(text: string): string {
  return text.replace(/[&<>"]/g, (char) => `&#${char.charCodeAt(0)};`);
}

function counter(headline: Headline, className: string): string {
  const { value, decimals, suffix } = headline;
  return `<span class="${className}" data-count="${value}" data-decimals="${decimals}" data-suffix="${escape(suffix)}">${escape(headlineText(headline))}</span>`;
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

function renderPanels(figure: Figure): string {
  let order = 0;
  return figure.panels
    .map((panel) => {
      const rows = panel.bars.map((bar) => {
        const kraai = bar.tool === "kraai";
        return `
          <li class="bar-row${kraai ? " is-kraai" : ""}" style="--fraction: ${barFraction(panel, bar).toFixed(4)}; --order: ${order++}">
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
          <p class="panel-head">${counter(panel.headline, "panel-number")}<span class="panel-caption">${escape(panel.caption)}</span></p>
          <ul class="bars">${rows.join("")}</ul>
        </div>`;
    })
    .join("");
}

export function renderNumbers(figures: Figure[]): string {
  const tabs = figures
    .map(
      (figure, index) => `
      <button class="claim" type="button" role="tab" id="claim-${figure.id}" aria-controls="chart-${figure.id}"
        aria-selected="${index === 0}" tabindex="${index === 0 ? 0 : -1}" data-claim="${index}">
        ${counter(figure.claim.headline, "claim-number")}
        <span class="claim-label">${escape(figure.claim.label)}</span>
      </button>`,
    )
    .join("");
  const charts = figures
    .map(
      (figure, index) => `
      <div class="chart" role="tabpanel" id="chart-${figure.id}" aria-labelledby="claim-${figure.id}" data-chart="${index}"${index === 0 ? "" : " hidden"}>
        <p class="chart-title">${escape(figure.title)}</p>
        <p class="visually-hidden">${escape(describe(figure))}</p>
        <div class="panels" style="--panels: ${figure.panels.length}" aria-hidden="true">${renderPanels(figure)}</div>
        <p class="fine-print">${figure.note}</p>
      </div>`,
    )
    .join("");
  return `
    <div class="claims" role="tablist" aria-label="Benchmarks">${tabs}<span class="claim-indicator" aria-hidden="true"></span></div>
    <div class="charts">${charts}</div>`;
}
