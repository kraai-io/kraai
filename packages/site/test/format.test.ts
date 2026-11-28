import assert from "node:assert/strict";
import test from "node:test";
import { barFraction, easeOut, headlineText } from "../src/format.ts";
import { codingTasks, resources, terminalBench } from "../src/stats.ts";

test("headlines count up to their final text", () => {
  const memory = resources.panels[0].headline;
  assert.equal(headlineText(memory, 0), "0.0×");
  assert.equal(headlineText(memory, 0.5), "3.4×");
  assert.equal(headlineText(memory), "6.7×");
  assert.equal(headlineText(memory, 2), "6.7×");
  assert.equal(headlineText(codingTasks.panels[0].headline), "53%");
});

test("the count-up ease starts at 0 and ends at 1", () => {
  assert.equal(easeOut(0), 0);
  assert.equal(easeOut(1), 1);
  assert.ok(easeOut(0.5) > 0.9);
});

test("bars are drawn against the largest bar unless a maximum is set", () => {
  const memory = resources.panels[0];
  assert.equal(barFraction(memory, memory.bars[1]), 1);
  assert.equal(barFraction(memory, memory.bars[0]), 40 / 274);

  const passRate = terminalBench.panels[2];
  assert.equal(barFraction(passRate, passRate.bars[0]), 0.9);
  assert.equal(barFraction(passRate, passRate.bars[1]), 0.94);
});
