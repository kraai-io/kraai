// Scroll reveals, number count-ups, and the benchmark tabs. The movement itself
// lives in CSS transitions; this file decides when each piece starts.
import { easeOut, headlineText } from "./format.ts";

const COUNT_MS = 1400;
const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
const running = new WeakMap<HTMLElement, number>();

function headlineOf(element: HTMLElement) {
  return {
    value: Number(element.dataset.count),
    decimals: Number(element.dataset.decimals),
    suffix: element.dataset.suffix ?? "",
  };
}

/** Counts every number inside `root` up from zero. */
function countUp(root: ParentNode, delay = 0): void {
  for (const element of root.querySelectorAll<HTMLElement>("[data-count]")) {
    const headline = headlineOf(element);
    if (reduce) {
      element.textContent = headlineText(headline);
      continue;
    }
    cancelAnimationFrame(running.get(element) ?? 0);
    element.textContent = headlineText(headline, 0);
    let start = 0;
    const frame = (now: number) => {
      if (!start) start = now + delay;
      const progress = Math.max(0, Math.min((now - start) / COUNT_MS, 1));
      element.textContent = headlineText(headline, easeOut(progress));
      if (progress < 1) running.set(element, requestAnimationFrame(frame));
    };
    running.set(element, requestAnimationFrame(frame));
  }
}

/** Replays a chart's bar growth from empty. */
function grow(chart: HTMLElement): void {
  chart.classList.remove("is-in");
  void chart.offsetWidth;
  chart.classList.add("is-in");
}

function onceVisible(target: Element, callback: () => void, threshold = 0.2): void {
  if (!("IntersectionObserver" in window)) {
    callback();
    return;
  }
  const observer = new IntersectionObserver(
    (entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      observer.disconnect();
      callback();
    },
    { rootMargin: "0px 0px -10% 0px", threshold },
  );
  observer.observe(target);
}

function setUpNumbers(section: HTMLElement): void {
  const claims = [...section.querySelectorAll<HTMLButtonElement>("[data-claim]")];
  const charts = [...section.querySelectorAll<HTMLElement>("[data-chart]")];
  const indicator = section.querySelector<HTMLElement>(".claim-indicator");
  let active = 0;
  let seen = false;

  const place = () => {
    const claim = claims[active];
    if (!indicator || !claim) return;
    indicator.style.width = `${claim.offsetWidth}px`;
    indicator.style.transform = `translate(${claim.offsetLeft}px, ${claim.offsetTop + claim.offsetHeight - 1}px)`;
  };

  const select = (index: number, focus: boolean) => {
    if (index === active) return;
    active = index;
    claims.forEach((claim, each) => {
      const selected = each === index;
      claim.setAttribute("aria-selected", String(selected));
      claim.tabIndex = selected ? 0 : -1;
    });
    charts.forEach((chart, each) => {
      chart.hidden = each !== index;
    });
    place();
    if (focus) claims[index].focus();
    if (seen) {
      grow(charts[index]);
      countUp(charts[index], 120);
    }
  };

  claims.forEach((claim, index) => {
    claim.addEventListener("click", () => select(index, false));
    claim.addEventListener("keydown", (event) => {
      const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
      if (!step) return;
      event.preventDefault();
      select((index + step + claims.length) % claims.length, true);
    });
  });

  place();
  new ResizeObserver(place).observe(section);

  if (!reduce) {
    for (const element of section.querySelectorAll<HTMLElement>("[data-count]")) {
      element.textContent = headlineText(headlineOf(element), 0);
    }
  }
  onceVisible(section.querySelector(".claims") ?? section, () => {
    seen = true;
    section.classList.add("is-in");
    countUp(section.querySelector(".claims") ?? section);
    grow(charts[active]);
    countUp(charts[active], 200);
  });
}

export function startMotion(): void {
  for (const target of document.querySelectorAll<HTMLElement>("[data-reveal]")) {
    onceVisible(target, () => target.classList.add("is-in"));
  }
  const numbers = document.querySelector<HTMLElement>("[data-numbers]");
  if (numbers) setUpNumbers(numbers);
}
