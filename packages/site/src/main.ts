import "@fontsource-variable/inter";
import "./style.css";
import { renderFigure } from "./charts.ts";
import { symbol, wordmark } from "./logos.ts";
import { startMotion } from "./motion.ts";
import { setUpDownloads } from "./downloads.ts";
import { codingTasks, resources, terminalBench, type Figure } from "./stats.ts";

const FIGURES: Record<string, Figure> = { resources, codingTasks, terminalBench };

for (const slot of document.querySelectorAll<HTMLElement>("[data-wordmark]")) slot.innerHTML = wordmark("wordmark-svg");
for (const slot of document.querySelectorAll<HTMLElement>("[data-symbol]")) slot.innerHTML = symbol("symbol-svg");

for (const card of document.querySelectorAll<HTMLElement>("[data-figure]")) {
  const figure = FIGURES[card.dataset.figure ?? ""];
  if (!figure) continue;
  const html = renderFigure(figure);
  const caption = card.querySelector("figcaption");
  if (caption) caption.insertAdjacentHTML("beforebegin", html);
  else card.insertAdjacentHTML("beforeend", html);
}

setUpTheme();
setUpDownloads();
startMotion();

function setUpTheme(): void {
  const root = document.documentElement;
  const toggle = document.querySelector<HTMLButtonElement>("[data-theme-toggle]");
  if (!toggle) return;
  const label = () => {
    toggle.setAttribute("aria-label", root.dataset.theme === "light" ? "Use dark theme" : "Use light theme");
  };
  label();
  toggle.addEventListener("click", () => {
    const next = root.dataset.theme === "light" ? "dark" : "light";
    root.dataset.theme = next;
    label();
    try {
      localStorage.setItem("kraai-theme", next);
    } catch {
      // Private windows can refuse storage; the choice just won't be remembered.
    }
  });
}
