#!/usr/bin/env bun
// The tree models the client draws up close (`systems::tree_models`), one per
// `TreeForm`, each at two levels of detail, generated with EZ-Tree
// (Daniel Greenheck, MIT) and written as GLB with the textures embedded.
//
//   cd tools/tree-models && bun install && bun gen.ts [--out DIR] [--preview /tmp/trees.png]
//
// Deterministic: a preset, a seed and the overrides below always give the same
// tree, so the assets can be regenerated rather than hand-edited. Every model
// stands on y = 0 and is scaled to **1 m tall**; the client scales it to the
// tree's height.
//
// EZ-Tree runs on three.js and wants a DOM only for loading its textures, so its
// texture module is stubbed and the images are embedded straight from the
// package. Bark: Poly Haven and texturecan (CC0); leaves: the EZ-Tree repository
// (MIT).
import { plugin } from "bun";
import { dirname, join } from "node:path";
import { parseArgs } from "node:util";

const { values: args } = parseArgs({
  args: Bun.argv.slice(2),
  options: { out: { type: "string" }, preview: { type: "string" } },
});
const out = args.out ?? join(import.meta.dir, "..", "..", "assets", "models", "trees");

const pkg = join(dirname(Bun.resolveSync("@dgreenheck/ez-tree", import.meta.dir)), "..");
const lib = join(pkg, "src", "lib");
plugin({
  name: "ez-tree-without-a-dom",
  setup(build) {
    build.onLoad({ filter: /ez-tree.*[\\/]src[\\/]lib[\\/]textures\.js$/ }, () => ({
      contents: "export const getBarkTexture = () => null; export const getLeafTexture = () => null;",
      loader: "js",
    }));
  },
});
const { Tree } = await import(join(lib, "tree.js"));
const { TreePreset } = await import(join(lib, "presets", "index.js"));
const { Document, NodeIO } = await import("@gltf-transform/core");

type Overrides = Record<string, any>;
/** One form: the preset it starts from, its seed, and what it changes. */
const FORMS: Record<string, { preset: string; seed: number; set?: Overrides }> = {
  // No species known: a plain broadleaf.
  round: { preset: "Ash Medium", seed: 13, set: { leaves: { tint: 0xffffff, count: 10 } } },
  // Oak, ash, lime, maple, elm, plane: a wide crown of several masses.
  broad: { preset: "Oak Medium", seed: 7 },
  // Birch, alder, rowan, pear: a narrow oval on a pale stem. The aspen preset's
  // leaves are tinted autumn yellow; white gives the texture's own green.
  upright: { preset: "Aspen Medium", seed: 3, set: { leaves: { tint: 0xffffff } } },
  // Lombardy poplar, fastigiate hornbeam: short branches held close to the stem.
  columnar: {
    preset: "Aspen Medium",
    seed: 21,
    set: {
      bark: { type: "oak" },
      branch: {
        angle: { 1: 18, 2: 15 },
        children: { 0: 22, 1: 3 },
        length: { 0: 50, 1: 7, 2: 4 },
        start: { 1: 0.12 },
        force: { strength: 0.03 },
      },
      leaves: { type: "ash", tint: 0xffffff, count: 9 },
    },
  },
  // No weeping willow: EZ-Tree cannot hang a curtain of branches, and the
  // willow keeps its own crown at every distance (`tree_models::has_model`).
  // Spruce, fir, thuja, larch.
  conical: { preset: "Pine Medium", seed: 5, set: { leaves: { count: 16 } } },
  // Scots pine: a bare stem and a flat, open crown high on it.
  pine: {
    preset: "Pine Large",
    seed: 9,
    set: {
      branch: { children: { 0: 40 }, start: { 1: 0.55 }, angle: { 1: 80 }, length: { 1: 26, 2: 12 } },
      leaves: { count: 14 },
    },
  },
};

/** The far version of a tree: the same tree with a third of the leaf cards,
 * larger so the crown keeps its mass, one card per leaf instead of a crossed
 * pair, and coarser branches. */
const LOD1: Overrides = { leavesCountScale: 0.25, leavesSizeScale: 1.7, billboard: "single", segmentsScale: 0.5 };

function merge(target: any, src: any) {
  for (const [k, v] of Object.entries(src)) {
    if (v && typeof v === "object" && !Array.isArray(v)) merge((target[k] ??= {}), v);
    else target[k] = v;
  }
  return target;
}

const linear = (hex: number) =>
  [16, 8, 0].map((s) => {
    const c = ((hex >> s) & 255) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });

const io = new NodeIO();
await Bun.$`mkdir -p ${out}`.quiet();
const preview: { name: string; tris: number[][] }[] = [];

for (const [form, spec] of Object.entries(FORMS)) {
  for (const lod of [0, 1]) {
    const options = merge(structuredClone(TreePreset[spec.preset]), spec.set ?? {});
    options.seed = spec.seed;
    if (lod === 1) {
      options.leaves.count = Math.max(2, Math.round(options.leaves.count * LOD1.leavesCountScale));
      options.leaves.size *= LOD1.leavesSizeScale;
      options.leaves.billboard = LOD1.billboard;
      for (const k of Object.keys(options.branch.segments))
        options.branch.segments[k] = Math.max(3, Math.round(options.branch.segments[k] * LOD1.segmentsScale));
      for (const k of Object.keys(options.branch.sections))
        options.branch.sections[k] = Math.max(2, Math.round(options.branch.sections[k] * LOD1.segmentsScale));
    }
    const tree = new Tree();
    tree.options.copy(options);
    tree.generate();

    const bark = tree.branchesMesh.geometry;
    const leaves = tree.leavesMesh.geometry;
    const barkPos = bark.attributes.position.array as Float32Array;
    const leafPos = leaves.attributes.position.array as Float32Array;
    let top = 0;
    for (const a of [barkPos, leafPos]) for (let i = 1; i < a.length; i += 3) top = Math.max(top, a[i]);
    const unit = 1 / top;

    const doc = new Document();
    const buffer = doc.createBuffer();
    const mesh = doc.createMesh(form);
    doc.createScene().addChild(doc.createNode(form).setMesh(mesh));
    const accessor = (type: "VEC2" | "VEC3" | "SCALAR", array: Float32Array | Uint32Array) =>
      doc.createAccessor().setType(type).setArray(array).setBuffer(buffer);
    const texture = async (path: string, mime: string) =>
      doc.createTexture().setImage(new Uint8Array(await Bun.file(path).arrayBuffer())).setMimeType(mime);

    // Bark: the package scales its UVs through the texture's repeat.
    {
      const pos = barkPos.map((v) => v * unit);
      const uv = new Float32Array(bark.attributes.uv.array);
      const s = options.bark.textureScale ?? { x: 1, y: 1 };
      for (let i = 0; i < uv.length; i += 2) {
        uv[i] *= s.x;
        uv[i + 1] *= s.y;
      }
      const type = options.bark.type;
      const material = doc
        .createMaterial("bark")
        .setBaseColorFactor([...linear(options.bark.tint), 1] as any)
        .setRoughnessFactor(0.95)
        .setMetallicFactor(0)
        .setBaseColorTexture(await texture(join(lib, "assets", "bark", `${type}_color_1k.jpg`), "image/jpeg"))
        .setNormalTexture(await texture(join(lib, "assets", "bark", `${type}_normal_1k.jpg`), "image/jpeg"));
      mesh.addPrimitive(
        doc
          .createPrimitive()
          .setAttribute("POSITION", accessor("VEC3", pos))
          .setAttribute("NORMAL", accessor("VEC3", new Float32Array(bark.attributes.normal.array)))
          .setAttribute("TEXCOORD_0", accessor("VEC2", uv))
          .setIndices(accessor("SCALAR", new Uint32Array(bark.index.array)))
          .setMaterial(material),
      );
    }
    // Leaves: normals point out from the crown's centre, so hundreds of cards
    // shade as one soft mass instead of as flat planes facing every way.
    {
      const pos = leafPos.map((v) => v * unit);
      let cy = 0;
      for (let i = 1; i < pos.length; i += 3) cy += pos[i];
      cy /= pos.length / 3;
      const normal = new Float32Array(pos.length);
      for (let i = 0; i < pos.length; i += 3) {
        const [x, y, z] = [pos[i], pos[i + 1] - cy, pos[i + 2]];
        const l = Math.hypot(x, y, z) || 1;
        normal.set([x / l, y / l, z / l], i);
      }
      const material = doc
        .createMaterial("leaves")
        .setBaseColorFactor([...linear(options.leaves.tint), 1] as any)
        .setRoughnessFactor(0.8)
        .setMetallicFactor(0)
        .setAlphaMode("MASK")
        .setAlphaCutoff(options.leaves.alphaTest ?? 0.5)
        .setDoubleSided(true)
        .setBaseColorTexture(await texture(join(lib, "assets", "leaves", `${options.leaves.type}_color.png`), "image/png"));
      mesh.addPrimitive(
        doc
          .createPrimitive()
          .setAttribute("POSITION", accessor("VEC3", pos))
          .setAttribute("NORMAL", accessor("VEC3", normal))
          .setAttribute("TEXCOORD_0", accessor("VEC2", new Float32Array(leaves.attributes.uv.array)))
          .setIndices(accessor("SCALAR", new Uint32Array(leaves.index.array)))
          .setMaterial(material),
      );
      if (args.preview) {
        const tris: number[][] = [];
        const add = (p: Float32Array, idx: ArrayLike<number>) => {
          for (let i = 0; i < idx.length; i += 3)
            tris.push([0, 1, 2].flatMap((k) => [p[idx[i + k] * 3], p[idx[i + k] * 3 + 1]]));
        };
        add(barkPos.map((v) => v * unit), bark.index.array);
        add(pos, leaves.index.array);
        preview.push({ name: `${form} ${lod}`, tris });
      }
    }

    const name = `${form}_lod${lod}.glb`;
    await io.write(join(out, name), doc);
    let radius = 0;
    for (const a of [barkPos, leafPos]) for (let i = 0; i < a.length; i += 3) radius = Math.max(radius, Math.hypot(a[i], a[i + 2]) * unit);
    const tris = (bark.index.count + leaves.index.count) / 3;
    console.log(`${name.padEnd(18)} ${String(tris).padStart(6)} tris   radius ${radius.toFixed(2)} × height`);
  }
}

// A side view of every model, to judge silhouettes without starting the client.
if (args.preview) {
  const cell = 220;
  const mvg = preview.map(({ tris }, i) => {
    const ox = (i % 2) * cell + cell / 2;
    const oy = Math.floor(i / 2) * cell + cell - 10;
    const s = cell - 30;
    return tris
      .map((t) => `polygon ${[0, 2, 4].map((k) => `${(ox + t[k] * s).toFixed(1)},${(oy - t[k + 1] * s).toFixed(1)}`).join(" ")}`)
      .join("\n");
  });
  const rows = Math.ceil(preview.length / 2);
  await Bun.write(
    "/tmp/tree-models.mvg",
    `viewbox 0 0 ${cell * 2} ${cell * rows}\nfill #e8eef2\nrectangle 0,0 ${cell * 2},${cell * rows}\nfill #3a5a30\n${mvg.join("\n")}\n`,
  );
  const labels = preview.map(({ name }, i) => `-annotate +${(i % 2) * cell + 6}+${Math.floor(i / 2) * cell + 16} "${name}"`).join(" ");
  await Bun.$`convert mvg:/tmp/tree-models.mvg ${{ raw: labels }} ${args.preview}`;
  console.log(`preview: ${args.preview}`);
}
