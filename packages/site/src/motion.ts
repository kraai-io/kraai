// Scroll reveals and number count-ups. The movement itself lives in CSS
// transitions; this file only decides when each element comes into view.
import { easeOut, headlineText } from "./format.ts";

const COUNT_MS = 1400;
// Matches the delay before the first bar starts to grow in style.css.
const COUNT_DELAY_MS = 150;

function headlineOf(element: HTMLElement) {
  return {
    value: Number(element.dataset.count),
    decimals: Number(element.dataset.decimals),
    suffix: element.dataset.suffix ?? "",
  };
}

function countUp(element: HTMLElement): void {
  const headline = headlineOf(element);
  let start = 0;
  const frame = (now: number) => {
    if (!start) start = now;
    const progress = Math.min((now - start) / COUNT_MS, 1);
    element.textContent = headlineText(headline, easeOut(progress));
    if (progress < 1) requestAnimationFrame(frame);
  };
  setTimeout(() => requestAnimationFrame(frame), COUNT_DELAY_MS);
}

export function startMotion(): void {
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const targets = document.querySelectorAll<HTMLElement>("[data-reveal]");
  const headlines = document.querySelectorAll<HTMLElement>(".headline[data-count]");

  if (!reduce) {
    for (const headline of headlines) headline.textContent = headlineText(headlineOf(headline), 0);
  }

  const reveal = (target: Element) => {
    target.classList.add("is-in");
    if (reduce) return;
    for (const headline of target.querySelectorAll<HTMLElement>(".headline[data-count]")) countUp(headline);
  };

  if (!("IntersectionObserver" in window)) {
    targets.forEach(reveal);
    return;
  }

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        observer.unobserve(entry.target);
        reveal(entry.target);
      }
    },
    // Wait until a card is properly on screen, so its bars grow where they can be seen.
    { rootMargin: "0px 0px -10% 0px", threshold: 0.2 },
  );
  targets.forEach((target) => observer.observe(target));
}
