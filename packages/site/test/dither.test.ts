import assert from "node:assert/strict";
import test from "node:test";
import { bayer, shapeFromSvg } from "../src/dither.ts";

test("the Bayer matrix uses every threshold once", () => {
  const matrix = bayer(8);
  assert.equal(matrix.length, 64);
  const sorted = [...matrix].sort((a, b) => a - b);
  sorted.forEach((value, index) => assert.ok(Math.abs(value - (index + 0.5) / 64) < 1e-6));
  // Neighbouring cells sit far apart in the order, which is what makes the pattern even.
  assert.equal(bayer(2).join(","), [0.125, 0.625, 0.875, 0.375].join(","));
});

test("reads the paths and viewBox from an SVG", () => {
  const shape = shapeFromSvg('<svg viewBox="7 -0.11 340 241.11"><g><path d="M0 0L1 1Z"/><path d="M2 2H3Z"/></g></svg>');
  assert.deepEqual(shape.viewBox, [7, -0.11, 340, 241.11]);
  assert.deepEqual(shape.paths, ["M0 0L1 1Z", "M2 2H3Z"]);
});
