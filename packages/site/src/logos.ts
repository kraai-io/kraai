// Single-color logos, inlined so they take the text color through currentColor.
// The Codex and Claude logos come from LobeHub's icon set (MIT).
import kraaiSymbol from "../../../assets/brand/svg/kraai-symbol-currentColor.svg?raw";
import kraaiWordmark from "../../../assets/brand/svg/kraai-logo-currentColor.svg?raw";
import codex from "../../../assets/readme/logos/codex.svg?raw";
import claude from "../../../assets/readme/logos/claude.svg?raw";
import type { Tool } from "./stats.ts";

/** Strips a logo file's title and accessible name, and marks it decorative. */
function decorative(source: string, className: string): string {
  return source
    .replace(/<title>.*?<\/title>/s, "")
    .replace(/\s(role|aria-label)="[^"]*"/g, "")
    .replace("<svg", `<svg class="${className}" aria-hidden="true" focusable="false"`);
}

const TOOL_LOGOS: Record<Tool, string> = { kraai: kraaiSymbol, codex, claude };

export function toolLogo(tool: Tool): string {
  return decorative(TOOL_LOGOS[tool], "tool-logo");
}

export function wordmark(className: string): string {
  return decorative(kraaiWordmark, className);
}

export function symbol(className: string): string {
  return decorative(kraaiSymbol, className);
}
