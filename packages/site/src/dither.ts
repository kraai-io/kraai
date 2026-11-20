// The hero's crow, drawn as a 1-bit ordered dither on a canvas.
//
// The bird and its halo are painted once into a small brightness field, one
// pixel per dither cell. Each frame thresholds the bird against an 8×8 Bayer
// matrix and the dust around it against random noise, and draws a square dot
// for every cell that passes. The dots fill in
// along the bird on load, a soft light follows the pointer, and the bird thins
// out as the hero scrolls away. Nothing is drawn while nothing changes.

const BAYER = bayer(8);

/** Thresholds in (0, 1) for an n×n ordered dither, n a power of two. */
export function bayer(n: number): Float32Array {
  let matrix = [[0]];
  for (let size = 1; size < n; size *= 2) {
    const next: number[][] = [];
    for (let y = 0; y < size * 2; y++) {
      next.push([]);
      for (let x = 0; x < size * 2; x++) {
        const quadrant = [0, 2, 3, 1][(y >= size ? 2 : 0) + (x >= size ? 1 : 0)];
        next[y].push(4 * matrix[y % size][x % size] + quadrant);
      }
    }
    matrix = next;
  }
  return Float32Array.from(matrix.flat(), (value) => (value + 0.5) / (n * n));
}

/** A repeatable 0–1 value per cell, so the drawing order looks hand-placed but never flickers. */
function hash(x: number, y: number): number {
  const s = Math.sin(x * 127.1 + y * 311.7) * 43758.5453;
  return s - Math.floor(s);
}

function clamp01(value: number): number {
  return value < 0 ? 0 : value > 1 ? 1 : value;
}

export interface Shape {
  paths: string[];
  viewBox: [number, number, number, number];
}

/** Reads the path data and viewBox from an SVG file's source. */
export function shapeFromSvg(source: string): Shape {
  const viewBox = source.match(/viewBox="([^"]+)"/)?.[1].split(/[\s,]+/).map(Number) ?? [0, 0, 1, 1];
  const paths = [...source.matchAll(/\sd="([^"]+)"/g)].map((match) => match[1]);
  return { paths, viewBox: viewBox as Shape["viewBox"] };
}

const DRAW_MS = 2600;

export function mountDither(canvas: HTMLCanvasElement, shape: Shape, scrollRoot: HTMLElement): void {
  const context = canvas.getContext("2d");
  if (!context) return;
  const ctx = context;
  const reduce = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const finePointer = matchMedia("(hover: hover) and (pointer: fine)").matches;

  let cols = 0;
  let rows = 0;
  let cell = 5;
  let dot = 3;
  let ratio = 1;
  let brightness = new Float32Array(0);
  let inside = new Uint8Array(0);
  let order = new Float32Array(0);
  let noise = new Float32Array(0);
  let ink = "#f5f5f7";
  let dust = "#86868b";

  let started = 0;
  let progress = reduce ? 1 : 0;
  let fade = 0;
  const light = { x: 0, y: 0, targetX: 0, targetY: 0, strength: 0, target: 0 };
  let frame = 0;
  let visible = true;

  function readColors(): void {
    const style = getComputedStyle(document.documentElement);
    ink = style.getPropertyValue("--dither-ink").trim() || ink;
    dust = style.getPropertyValue("--dither-dust").trim() || dust;
  }

  function layout(): void {
    const rect = canvas.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    ratio = Math.min(window.devicePixelRatio || 1, 2);
    const wide = rect.width >= 900;
    cell = wide ? 5 : 4;
    dot = wide ? 3 : 2.5;
    cols = Math.ceil(rect.width / cell);
    rows = Math.ceil(rect.height / cell);
    canvas.width = Math.round(rect.width * ratio);
    canvas.height = Math.round(rect.height * ratio);

    const [vx, vy, vw, vh] = shape.viewBox;
    const birdHeight = wide ? rows * 0.64 : Math.min(rows * 0.3, (cols * 0.86 * vh) / vw);
    const scale = birdHeight / vh;
    const birdWidth = vw * scale;
    const centerX = wide ? cols * 0.73 : cols * 0.5;
    const centerY = wide ? rows * 0.5 : rows * 0.24;
    const left = centerX - birdWidth / 2;
    const top = centerY - birdHeight / 2;

    const field = document.createElement("canvas");
    field.width = cols;
    field.height = rows;
    const paint = field.getContext("2d", { willReadFrequently: true });
    if (!paint) return;
    const paths = shape.paths.map((path) => new Path2D(path));
    const drawBird = (fill: string | CanvasGradient) => {
      paint.save();
      paint.translate(left, top);
      paint.scale(scale, scale);
      paint.translate(-vx, -vy);
      paint.fillStyle = fill;
      for (const path of paths) paint.fill(path);
      paint.restore();
    };

    // Pass 1: where the bird is.
    paint.clearRect(0, 0, cols, rows);
    drawBird("#ffffff");
    const mask = paint.getImageData(0, 0, cols, rows).data;

    // Pass 2: how bright each cell is. A halo of sparse dust, then the bird lit from its beak.
    paint.fillStyle = "#000000";
    paint.fillRect(0, 0, cols, rows);
    const halo = paint.createRadialGradient(centerX, centerY, 0, centerX, centerY, birdWidth * 0.62);
    halo.addColorStop(0, "rgba(255,255,255,0.16)");
    halo.addColorStop(0.55, "rgba(255,255,255,0.05)");
    halo.addColorStop(1, "rgba(255,255,255,0)");
    paint.fillStyle = halo;
    paint.fillRect(0, 0, cols, rows);
    const shade = paint.createLinearGradient(vx + vw, vy, vx, vy + vh);
    shade.addColorStop(0, "#ffffff");
    shade.addColorStop(0.4, "#b8b8b8");
    shade.addColorStop(1, "#262626");
    drawBird(shade);
    const pixels = paint.getImageData(0, 0, cols, rows).data;

    brightness = new Float32Array(cols * rows);
    inside = new Uint8Array(cols * rows);
    order = new Float32Array(cols * rows);
    noise = new Float32Array(cols * rows);
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const i = y * cols + x;
        brightness[i] = pixels[i * 4] / 255;
        inside[i] = mask[i * 4 + 3] > 127 ? 1 : 0;
        const jitter = hash(x, y);
        noise[i] = hash(x + 913, y + 271);
        // The bird is drawn from its tail to its beak; the dust settles in around it afterwards.
        order[i] = inside[i]
          ? clamp01((x - left) / birdWidth) * 0.62 + clamp01(1 - (y - top) / birdHeight) * 0.18 + jitter * 0.12
          : 0.35 + jitter * 0.6;
      }
    }
    light.x = light.targetX = cols * 0.7;
    light.y = light.targetY = rows * 0.5;
  }

  function draw(): void {
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, cols * cell, rows * cell);
    const offset = (cell - dot) / 2;
    const radius = Math.max(cols, rows) * 0.08;
    const reach = 1 / (radius * radius);
    const front = progress * 1.2;

    for (let pass = 0; pass < 2; pass++) {
      ctx.fillStyle = pass === 0 ? dust : ink;
      for (let y = 0; y < rows; y++) {
        const threshold = (y % 8) * 8;
        const dy = y - light.y;
        for (let x = 0; x < cols; x++) {
          const i = y * cols + x;
          if (inside[i] !== pass) continue;
          const reveal = clamp01((front - order[i]) / 0.1);
          if (reveal === 0) continue;
          let value = brightness[i] * reveal;
          // Fresh ink is brighter for a moment, so the drawing edge sparkles.
          if (pass === 1) value += reveal * (1 - reveal) * 1.6;
          if (light.strength > 0.001) {
            const dx = x - light.x;
            value += light.strength * 0.42 * Math.exp(-(dx * dx + dy * dy) * reach);
          }
          value *= 1 - fade;
          if (value > (pass === 1 ? BAYER[threshold + (x % 8)] : noise[i])) ctx.fillRect(x * cell + offset, y * cell + offset, dot, dot);
        }
      }
    }
  }

  function tick(now: number): void {
    frame = 0;
    if (!started) started = now;
    if (progress < 1) progress = Math.min((now - started) / DRAW_MS, 1);
    light.x += (light.targetX - light.x) * 0.12;
    light.y += (light.targetY - light.y) * 0.12;
    light.strength += (light.target - light.strength) * 0.08;
    draw();
    const settled =
      progress >= 1 &&
      Math.abs(light.targetX - light.x) < 0.2 &&
      Math.abs(light.targetY - light.y) < 0.2 &&
      Math.abs(light.target - light.strength) < 0.005;
    if (!settled) schedule();
  }

  function schedule(): void {
    if (!frame && visible) frame = requestAnimationFrame(tick);
  }

  function updateFade(): void {
    const height = scrollRoot.offsetHeight || 1;
    fade = reduce ? 0 : clamp01(window.scrollY / (height * 0.85)) ** 1.4;
  }

  readColors();
  layout();
  updateFade();
  if (reduce) draw();
  else schedule();

  let resizeTimer = 0;
  new ResizeObserver(() => {
    clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => {
      layout();
      draw();
    }, 120);
  }).observe(canvas);

  new MutationObserver(() => {
    readColors();
    draw();
  }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });

  new IntersectionObserver(([entry]) => {
    visible = entry.isIntersecting;
    if (visible) schedule();
  }).observe(canvas);

  if (reduce) return;

  window.addEventListener(
    "scroll",
    () => {
      updateFade();
      schedule();
    },
    { passive: true },
  );

  if (!finePointer) return;
  scrollRoot.addEventListener("pointermove", (event) => {
    const rect = canvas.getBoundingClientRect();
    light.targetX = (event.clientX - rect.left) / cell;
    light.targetY = (event.clientY - rect.top) / cell;
    light.target = 1;
    schedule();
  });
  scrollRoot.addEventListener("pointerleave", () => {
    light.target = 0;
    schedule();
  });
}
