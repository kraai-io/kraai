import type { Bar, Headline, Panel } from "./stats.ts";

/** A headline number partway through its count-up; `progress` runs from 0 to 1. */
export function headlineText(headline: Headline, progress = 1): string {
  const value = headline.value * Math.min(Math.max(progress, 0), 1);
  return `${value.toFixed(headline.decimals)}${headline.suffix}`;
}

/** Exponential ease-out: fast at first, then settling, like the CSS --ease-out curve. */
export function easeOut(t: number): number {
  return t >= 1 ? 1 : 1 - 2 ** (-10 * t);
}

/** A bar's length as a share of the panel's longest possible bar. */
export function barFraction(panel: Panel, bar: Bar): number {
  const maximum = panel.maximum ?? Math.max(...panel.bars.map((each) => each.value));
  return maximum > 0 ? bar.value / maximum : 0;
}
