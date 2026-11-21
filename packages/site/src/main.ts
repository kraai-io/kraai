import "@fontsource-variable/inter";
import "./style.css";
import { renderNumbers } from "./charts.ts";
import { mountDither, shapeFromSvg } from "./dither.ts";
import { setUpDownloads } from "./downloads.ts";
import { kraaiSymbolSource, wordmark } from "./logos.ts";
import { startMotion } from "./motion.ts";
import { figures } from "./stats.ts";

for (const slot of document.querySelectorAll<HTMLElement>("[data-wordmark]")) slot.innerHTML = wordmark("wordmark");
document.querySelector("[data-numbers]")?.insertAdjacentHTML("beforeend", renderNumbers(figures));

const canvas = document.querySelector<HTMLCanvasElement>("[data-dither]");
const hero = document.querySelector<HTMLElement>("[data-hero]");
if (canvas && hero) mountDither(canvas, shapeFromSvg(kraaiSymbolSource), hero);

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
