---
topic: vegetation
triggers: [tree_form.rs, TreeForm, TREE_FORMS, from_taxon, tree_forms, LATIJNSE_NAAM, "tree model", "tree models", "tree species", boomsoort, build_vegetation_meshes, push_trunk, "leaf_type", genus]
updated: 2026-11-07
---

# A tree is drawn by its species

Until 2026-11-07 every tree in the client was one lumpy ball of 18 vertices on a
square post. Height and width varied; the shape never did, so a canal lined with
limes, a row of Lombardy poplars and a stand of spruce were the same object at
three sizes. `cartopolis_geo::tree_form` now gives each surveyed tree one of seven
silhouettes, chosen by its species.

## The species is in the data, for nearly every street tree

Groningen's tree register (`geo-data:Bomen gemeente Groningen`) carries
`BOOMSOORT`, `LATIJNSE_NAAM`, `KIEMJAAR` and `MONUMENTAAL` beside the height band
atlas already read. Fetched whole on 2026-11-07: **126,016 trees, 155 without a
Latin name.** Through `TreeForm::from_taxon` they come out as:

| form | trees |
|---|---|
| broad (oak, ash, maple, lime, elm, plane …) | 96,855 |
| upright (birch, alder, rowan, pear …) | 21,098 |
| columnar (`'Fastigiata'`, `'Italica'`, `'Columella'` …) | 3,186 |
| conical (spruce, thuja, larch, metasequoia …) | 2,849 |
| pine | 991 |
| weeping (`Salix sepulcralis`, `'Pendula'` …) | 787 |
| none (`Verwijderd`, empty) | 250 |

The cultivar decides before the genus does — a `'Fastigiata'` oak is a column —
and is read only between the quotes, because `Betula pendula`, the silver birch,
is a species name and does not weep. OSM's `natural=tree` gives the same name in
`species` / `taxon` / `genus` where somebody mapped it, and the client reads it
there; `leaf_type=needleleaved` alone still makes a conifer.

The BGT registers no species, and the vector tiles' woods and parks are areas
with no trees in them at all, so **every scattered tree and every BGT tree stays
`Round`, which is the old ball vertex for vertex.** That is also what keeps the
cost down: a wood cell scatters up to 6,000 trees, and those did not grow.

## What it costs

`Round` is 18 vertices and 32 triangles; the largest form, `Broad`, three lobes of
the same sphere, is 54 and 96; the lathes (columnar, weeping, conical) are 34–42
vertices. The trunk went from a 4-sided post (16 vertices) to a 6-sided tapering
one (24), on every tree. Only surveyed trees take the dearer crowns — a dense
Groningen cell carries a few thousand — and the mesh builder still cuts each
cell's crowns to the frame's upload ceiling, now by each tree's own size rather
than one per-tree constant. `tree_form::tests::crowns_stay_small` holds the
ceiling at 54 vertices.

## The species rides in a key of its own

The register cell file (`formats::registers`) carries `tree_forms`, one slug per
tree, beside `trees`, rather than a fourth element in each tree row. `serde_json`
refuses an array longer than the tuple it reads into, so a fourth element would
have made every installed client — the phone's APK included — throw away the
whole cell, lamp posts and parking bays with the trees, the first time atlas
re-ran. An unknown key is ignored. A list that does not pair up with the trees is
ignored too (`Cell::tree_form`), since pairing half a list would put every
species on its neighbour.

**Nothing changes on screen until atlas re-runs `registers` on the host** — the
deploy ships the binary, not the data (`atlas.md`).

## Real 3D models: not taken yet, and why

The question that started this was whether to put real tree models in. Three
things stand in the way, and none of them is the model itself:

- **Count.** The street-object model path (`street-objects-can-wear-a-model.md`)
  spawns one entity per instance, which is affordable for 651 lamp posts on a
  page and not for thousands of trees a cell out to 2 km. Models need GPU
  instancing, which the client does not have.
- **Leaves.** A model's foliage is alpha-tested cards; `scatter.rs` chose opaque
  solids because cards collapse to lines from overhead, where this camera often
  is, and alpha needs a pipeline of its own.
- **The phone.** Mass mesh uploads are what lost the GPU device on the Adreno
  (`upload-bursts.md`).

The species above is also what a model path would key on, so if models come, they
come for the last ~150 m, chosen by `TreeForm` or by genus, with these
silhouettes beyond.
