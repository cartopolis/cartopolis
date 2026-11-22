// The graffiti sheet: real graffiti from Groningen, photographed for Wikimedia
// Commons, cut loose from its wall and packed into one texture the facade shader
// paints the walls of narrow streets with (`systems::graffiti`).
//
//   cd tools/graffiti && bun install && bun gen.ts
//
// writes `assets/textures/graffiti.png` (4 × 4 cells of 512 px), the cell table
// the shader reads (`assets/shaders/graffiti_cells.wesl`) and the credits every
// photograph needs (`assets/textures/graffiti.md`). Photos are fetched from
// Commons at 1600 px and kept in `.cache/` between runs.
//
// Three ways to cut one out, each per source:
//   key    — paint on a plain wall: the wall's colour is read off the crop's four
//            corners and blended across it, and a texel is paint by how far it
//            strays from that, with the wall unmixed out of its colour;
//   chroma — loud paint on a wall that is not plain (carved text, joints): a
//            texel is paint by how much more saturated it is than the wall;
//   patch  — a wall painted edge to edge: the crop itself, with a ragged border.

import sharp from "sharp";
import { existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const CELL = 512;
const GRID = 4;
const ROOT = join(import.meta.dir, "..", "..");
const CACHE = join(import.meta.dir, ".cache");

type Mode = "key" | "chroma" | "patch";
interface Source {
  /** The Commons file page title, without `File:`. */
  file: string;
  /** Crop as fractions of the photo: x0, y0, x1, y1. */
  crop: [number, number, number, number];
  mode: Mode;
  /** How wide it is on a wall, metres. */
  metres: number;
  /** A tag rather than a piece: the shader lays tags in their own layer. */
  tag?: boolean;
  /** `key`/`chroma` thresholds: below `lo` is wall, above `hi` paint. */
  lo?: number;
  hi?: number;
}

const SOURCES: Source[] = [
  // Pieces.
  { file: "20120413 Graffiti Hereweg 97 Groningen NL.jpg", crop: [0.2, 0.12, 0.92, 0.82], mode: "key", metres: 1.9 },
  { file: "20140718 Graffiti vm loskade Winschoterdiep Groningen NL (2).jpg", crop: [0.0, 0.06, 0.92, 0.99], mode: "key", metres: 2.3, lo: 40, hi: 85 },
  { file: "20120810 Graffiti vm loskade Winschoterdiep Groningen NL.jpg", crop: [0.24, 0.2, 0.8, 0.82], mode: "key", metres: 1.8, lo: 40, hi: 85 },
  { file: "20140718 Graffiti vm loskade Winschoterdiep Groningen NL (1).jpg", crop: [0.0, 0.14, 1.0, 0.9], mode: "key", metres: 2.8, lo: 40, hi: 85 },
  { file: "20140401 Hoendiepskade 21-22 Groningen NL.jpg", crop: [0.235, 0.7, 0.625, 0.855], mode: "key", metres: 1.8 },
  { file: "20120202 Graffiti Glaudé-locatie Groningen NL.jpg", crop: [0.28, 0.47, 0.74, 0.67], mode: "key", metres: 3.2 },
  { file: "20140629 Graffiti Ebbingekwartier Groningen NL (2).jpg", crop: [0.25, 0.4, 0.76, 0.77], mode: "patch", metres: 3.0 },
  { file: "20141102 Graffiti Glasfabriek Spaarnestraat Groningen NL.jpg", crop: [0.0, 0.32, 1.0, 0.86], mode: "patch", metres: 2.6 },
  { file: "Geee33589.jpg", crop: [0.0, 0.08, 1.0, 0.92], mode: "patch", metres: 3.0 },
  { file: "20120311 Glasfabriek Spaarnestraat Groningen NL.jpg", crop: [0.2, 0.62, 0.79, 0.82], mode: "patch", metres: 3.0 },
  { file: "Bekladde gevelsteen van de studentensociëteit in een van de achtergevels van de Grote Markt - Groningen - 20414086 - RCE.jpg", crop: [0.07, 0.36, 0.93, 0.76], mode: "chroma", metres: 1.4, lo: 12, hi: 40 },
  { file: "20120413 Graffiti Hereweg 97 Groningen NL.jpg", crop: [0.8, 0.12, 1.0, 0.5], mode: "key", metres: 0.8, tag: true },
  // Tags.
  { file: "20130715 Walfridusbrug Van Starkenborghkanaal Groningen NL (2).jpg", crop: [0.1, 0.55, 0.62, 0.87], mode: "key", metres: 1.1, tag: true },
  { file: "20120413 Graffiti Hereweg 97 Groningen NL.jpg", crop: [0.0, 0.14, 0.3, 0.52], mode: "key", metres: 0.9, tag: true },
  { file: "20120413 Graffiti Hereweg 97 Groningen NL.jpg", crop: [0.12, 0.56, 0.36, 0.7], mode: "key", metres: 0.6, tag: true },
];

interface Info {
  thumb: string;
  licence: string;
  licenceUrl: string;
  artist: string;
  page: string;
}

async function info(file: string): Promise<Info> {
  const url =
    "https://commons.wikimedia.org/w/api.php?action=query&prop=imageinfo&iiprop=url|extmetadata" +
    `&iiurlwidth=1600&format=json&titles=${encodeURIComponent("File:" + file)}`;
  const r = await fetch(url, { headers: { "User-Agent": "cartopolis-graffiti-sheet/1.0 (https://cartopolis.org)" } });
  const d: any = await r.json();
  const page: any = Object.values(d.query.pages)[0];
  const ii = page.imageinfo[0];
  const m = ii.extmetadata ?? {};
  const text = (k: string) => String(m[k]?.value ?? "").replace(/<[^>]+>/g, "").trim();
  return {
    thumb: ii.thumburl,
    licence: text("LicenseShortName"),
    licenceUrl: text("LicenseUrl"),
    artist: text("Artist"),
    page: ii.descriptionurl,
  };
}

async function photo(file: string, i: Info): Promise<Buffer> {
  mkdirSync(CACHE, { recursive: true });
  const path = join(CACHE, file.replace(/[^\w.-]+/g, "_"));
  if (!existsSync(path)) {
    const r = await fetch(i.thumb, { headers: { "User-Agent": "cartopolis-graffiti-sheet/1.0 (https://cartopolis.org)" } });
    if (!r.ok) throw new Error(`${file}: ${r.status}`);
    await Bun.write(path, await r.arrayBuffer());
  }
  return Buffer.from(await Bun.file(path).arrayBuffer());
}

const smooth = (lo: number, hi: number, x: number) => {
  const t = Math.min(Math.max((x - lo) / (hi - lo), 0), 1);
  return t * t * (3 - 2 * t);
};

/** Median of each channel over a box. */
function median(px: Uint8Array, w: number, x0: number, y0: number, x1: number, y1: number): number[] {
  const ch: number[][] = [[], [], []];
  for (let y = y0; y < y1; y++)
    for (let x = x0; x < x1; x++) for (let c = 0; c < 3; c++) ch[c].push(px[(y * w + x) * 3 + c]);
  return ch.map((v) => v.sort((a, b) => a - b)[v.length >> 1]);
}

/** Value noise in 0–1, for the ragged border of a patch. */
function noise(x: number, y: number): number {
  const h = (i: number, j: number) => {
    const s = Math.sin(i * 127.1 + j * 311.7) * 43758.5453;
    return s - Math.floor(s);
  };
  const xi = Math.floor(x), yi = Math.floor(y);
  const fx = x - xi, fy = y - yi;
  const u = fx * fx * (3 - 2 * fx), v = fy * fy * (3 - 2 * fy);
  const a = h(xi, yi), b = h(xi + 1, yi), c = h(xi, yi + 1), d = h(xi + 1, yi + 1);
  return a + (b - a) * u + (c - a) * v + (a - b - c + d) * u * v;
}

/** Cut one source out: RGBA, `w` × `h`. */
async function cut(s: Source, buf: Buffer): Promise<{ rgba: Uint8Array; w: number; h: number }> {
  const meta = await sharp(buf).metadata();
  const W = meta.width!, H = meta.height!;
  const left = Math.round(s.crop[0] * W), top = Math.round(s.crop[1] * H);
  const w = Math.round((s.crop[2] - s.crop[0]) * W), h = Math.round((s.crop[3] - s.crop[1]) * H);
  const px = new Uint8Array(
    await sharp(buf).extract({ left, top, width: w, height: h }).removeAlpha().raw().toBuffer(),
  );
  const out = new Uint8Array(w * h * 4);
  const sat = (c: ArrayLike<number>) => Math.max(c[0], c[1], c[2]) - Math.min(c[0], c[1], c[2]);
  const lo = s.lo ?? 35, hi = s.hi ?? 75;
  // The wall behind the paint, as it is lit: first the crop's four corners
  // blended across it, then three rounds of blurring the crop with the paint
  // found so far left out of the average — so a shadow or a light falling
  // across the wall is the wall, not paint.
  const k = Math.max(4, Math.round(Math.min(w, h) * 0.06));
  const tl = median(px, w, 0, 0, k, k), tr = median(px, w, w - k, 0, w, k);
  const bl = median(px, w, 0, h - k, k, h), br = median(px, w, w - k, h - k, w, h);
  const bg = new Float32Array(w * h * 3);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const fx = x / (w - 1), fy = y / (h - 1);
      for (let c = 0; c < 3; c++)
        bg[(y * w + x) * 3 + c] =
          (tl[c] * (1 - fx) + tr[c] * fx) * (1 - fy) + (bl[c] * (1 - fx) + br[c] * fx) * fy;
    }
  const alpha = new Float32Array(w * h);
  const measure = () => {
    for (let i = 0; i < w * h; i++) {
      const p = px.subarray(i * 3, i * 3 + 3);
      const b = bg.subarray(i * 3, i * 3 + 3);
      alpha[i] =
        s.mode === "chroma"
          ? smooth(lo, hi, sat(p) - sat(b))
          : smooth(lo, hi, Math.hypot(p[0] - b[0], p[1] - b[1], p[2] - b[2]));
    }
  };
  if (s.mode !== "patch") {
    const sigma = Math.max(3, Math.min(w, h) / 12);
    for (let round = 0; round < 3; round++) {
      measure();
      // Normalised convolution, blur(wall · weight) / blur(weight): the weight
      // as alpha, which sharp premultiplies by before it blurs and divides out
      // after.
      const weighted = new Uint8Array(w * h * 4);
      for (let i = 0; i < w * h; i++) {
        const wt = 1 - smooth(0.05, 0.3, alpha[i]);
        for (let c = 0; c < 3; c++) weighted[i * 4 + c] = px[i * 3 + c];
        weighted[i * 4 + 3] = Math.round(wt * 255);
      }
      const blurred = new Uint8Array(
        await sharp(Buffer.from(weighted), { raw: { width: w, height: h, channels: 4 } })
          .blur(sigma)
          .raw()
          .toBuffer(),
      );
      for (let i = 0; i < w * h; i++) {
        const wt = blurred[i * 4 + 3] / 255;
        // Where the paint covers everything near, keep what the last round had.
        if (wt < 0.04) continue;
        for (let c = 0; c < 3; c++) bg[i * 3 + c] = blurred[i * 4 + c];
      }
    }
    measure();
  }
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = y * w + x;
      const p = [px[i * 3], px[i * 3 + 1], px[i * 3 + 2]];
      let a = alpha[i];
      if (s.mode === "patch") {
        const edge = Math.min(x, y, w - 1 - x, h - 1 - y) / Math.min(w, h);
        a = smooth(0.0, 0.05, edge - 0.05 * noise(x / 9, y / 9) - 0.02 * noise(x / 2.5, y / 2.5));
      }
      // Unmix the wall out of a part-covered texel, so an edge is the paint's
      // colour at part coverage rather than paint and wall at full.
      const b = bg.subarray(i * 3, i * 3 + 3);
      const c = s.mode === "patch" || a < 0.02 ? p : p.map((v, j) => b[j] + (v - b[j]) / a);
      for (let j = 0; j < 3; j++) out[i * 4 + j] = Math.min(255, Math.max(0, Math.round(c[j])));
      out[i * 4 + 3] = Math.round(a * 255);
    }
  }
  return { rgba: out, w, h };
}

/** Spread colour into the clear texels round the paint, so the mip chain's
 * box filter does not pull a dark fringe in from transparent black. */
function bleed(px: Uint8Array, w: number, h: number) {
  let known = new Uint8Array(w * h);
  for (let i = 0; i < w * h; i++) known[i] = px[i * 4 + 3] > 8 ? 1 : 0;
  for (let pass = 0; pass < 24; pass++) {
    const next = known.slice();
    for (let y = 0; y < h; y++)
      for (let x = 0; x < w; x++) {
        const i = y * w + x;
        if (known[i]) continue;
        let r = 0, g = 0, b = 0, n = 0;
        for (let dy = -1; dy <= 1; dy++)
          for (let dx = -1; dx <= 1; dx++) {
            const xx = x + dx, yy = y + dy;
            if (xx < 0 || yy < 0 || xx >= w || yy >= h) continue;
            const j = yy * w + xx;
            if (!known[j]) continue;
            r += px[j * 4]; g += px[j * 4 + 1]; b += px[j * 4 + 2]; n++;
          }
        if (n > 0) {
          px[i * 4] = r / n; px[i * 4 + 1] = g / n; px[i * 4 + 2] = b / n;
          next[i] = 1;
        }
      }
    known = next;
  }
}

const SIZE = CELL * GRID;
const sheet = new Uint8Array(SIZE * SIZE * 4);
const cells: string[] = [];
const credits = new Map<string, Info>();
for (const [n, s] of SOURCES.entries()) {
  const i = await info(s.file);
  credits.set(s.file, i);
  const { rgba, w, h } = await cut(s, await photo(s.file, i));
  // Fitted into its cell, aspect kept, a 4 px margin so the mips do not bleed
  // into the neighbour.
  const scale = Math.min((CELL - 8) / w, (CELL - 8) / h);
  const fw = Math.round(w * scale), fh = Math.round(h * scale);
  const fitted = new Uint8Array(
    await sharp(Buffer.from(rgba), { raw: { width: w, height: h, channels: 4 } })
      .resize(fw, fh, { kernel: "lanczos3" })
      .raw()
      .toBuffer(),
  );
  const ox = (n % GRID) * CELL + ((CELL - fw) >> 1);
  const oy = Math.floor(n / GRID) * CELL + ((CELL - fh) >> 1);
  for (let y = 0; y < fh; y++)
    sheet.set(fitted.subarray(y * fw * 4, (y + 1) * fw * 4), ((oy + y) * SIZE + ox) * 4);
  cells.push(
    `    vec4<f32>(${(fw / CELL).toFixed(4)}, ${(fh / CELL).toFixed(4)}, ${s.metres.toFixed(2)}, ${s.tag ? 1 : 0}.0),`,
  );
  console.log(`${n}: ${s.file} → ${fw}×${fh} (${s.mode})`);
}
bleed(sheet, SIZE, SIZE);
mkdirSync(join(ROOT, "assets", "textures"), { recursive: true });
await sharp(Buffer.from(sheet), { raw: { width: SIZE, height: SIZE, channels: 4 } })
  // 256 colours, dithered: a megabyte where full colour is eight, which every
  // visit to the web client and every APK would carry, and no difference a
  // wall two metres off can show.
  .png({ palette: true, colours: 256, dither: 0.8, compressionLevel: 9 })
  .toFile(join(ROOT, "assets", "textures", "graffiti.png"));

const pieces = SOURCES.filter((s) => !s.tag).length;
await Bun.write(
  join(ROOT, "assets", "shaders", "graffiti_cells.wesl"),
  `// Written by tools/graffiti/gen.ts — do not edit; run it again.
//
// The cells of assets/textures/graffiti.png, ${GRID} × ${GRID}, row by row: x, y the
// share of the cell the cut-out fills (centred in it), z its width on a wall in
// metres, w 1 for a tag and 0 for a piece. The pieces come first.

const GRID: f32 = ${GRID}.0;
const PIECES: u32 = ${pieces}u;
const TAGS: u32 = ${SOURCES.length - pieces}u;
const CELLS = array<vec4<f32>, ${SOURCES.length}>(
${cells.join("\n")}
);
`,
);

const lines = [...credits.entries()].map(
  ([file, i]) => `- [${file}](${i.page}) — ${i.artist}, ${i.licence}${i.licenceUrl ? ` (${i.licenceUrl})` : ""}`,
);
await Bun.write(
  join(ROOT, "assets", "textures", "graffiti.md"),
  `# graffiti.png

Graffiti in Groningen, photographed for Wikimedia Commons and cut loose from its
walls by \`tools/graffiti/gen.ts\`. The sheet is a derivative of these photographs
and is shared under the same terms (CC BY-SA where they are; the public-domain one
is credited all the same). The paint itself is the work of its writers, unnamed.

${lines.join("\n")}
`,
);
console.log(`wrote the sheet, ${SOURCES.length} cells, ${credits.size} photographs credited`);
