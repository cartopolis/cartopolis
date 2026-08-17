#!/usr/bin/env python3
"""Rebuild `assets/fonts/MaterialSymbolsRounded-subset.ttf` from the icon table.

The table in `crates/cartopolis/src/systems/gui/icons.rs` is the single source of
truth: each row names a codepoint *and* Google's name for it. This script reads
those rows, checks every pair against the upstream `.codepoints` manifest — a
mistyped `\\u{...}` would otherwise ship a plausible-looking but wrong icon, and
nothing downstream could tell — instances the variable font at the one style the
app uses, and subsets it to exactly those glyphs.

Upstream is 15 MB; the result is around 20 kB, which is what makes it something
you can `include_bytes!` into an APK.

    pip install fonttools
    tools/subset-icon-font.py                 # downloads upstream into /tmp
    tools/subset-icon-font.py --font a.ttf --codepoints a.codepoints

Re-run it whenever a row is added to the table. `icons::tests::every_icon_has_a_
glyph` fails if you forget.
"""

from __future__ import annotations

import argparse
import io
import pathlib
import re
import sys
import urllib.request

REPO = pathlib.Path(__file__).resolve().parent.parent
TABLE = REPO / "crates/cartopolis/src/systems/gui/icons.rs"
OUT = REPO / "assets/fonts/MaterialSymbolsRounded-subset.ttf"

BASE = (
    "https://raw.githubusercontent.com/google/material-design-icons/master/variablefont/"
    "MaterialSymbolsRounded%5BFILL%2CGRAD%2Copsz%2Cwght%5D"
)

# The one style the app draws: outlined (FILL 0), no grade shift, optical size 24
# (the size icons are actually used at here), and a weight that sits beside Plus
# Jakarta Sans SemiBold without looking spindly next to it.
INSTANCE = {"FILL": 0, "GRAD": 0, "opsz": 24, "wght": 500}

ROW = re.compile(r'^\s*([A-Z][A-Z0-9_]*) = "\\u\{([0-9a-f]{4,6})\}", "([a-z0-9_]+)";', re.M)


def rows() -> list[tuple[str, int, str]]:
    src = TABLE.read_text(encoding="utf-8")
    found = [(k, int(cp, 16), name) for k, cp, name in ROW.findall(src)]
    if not found:
        sys.exit(f"no icon rows matched in {TABLE} — has the macro's shape changed?")
    return found


def fetch(url: str, cached: pathlib.Path) -> bytes:
    if cached.exists():
        return cached.read_bytes()
    print(f"downloading {url}")
    data = urllib.request.urlopen(url).read()
    cached.write_bytes(data)
    return data


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--font", type=pathlib.Path, help="upstream variable TTF")
    ap.add_argument("--codepoints", type=pathlib.Path, help="upstream .codepoints")
    args = ap.parse_args()

    from fontTools import subset
    from fontTools.ttLib import TTFont
    from fontTools.varLib import instancer

    cache = pathlib.Path("/tmp")
    font_bytes = (
        args.font.read_bytes()
        if args.font
        else fetch(BASE + ".ttf", cache / "MaterialSymbolsRounded.ttf")
    )
    cp_text = (
        args.codepoints.read_text()
        if args.codepoints
        else fetch(BASE + ".codepoints", cache / "MaterialSymbolsRounded.codepoints").decode()
    )

    upstream = {}
    for line in cp_text.splitlines():
        if line.strip():
            name, cp = line.split()
            upstream[name] = int(cp, 16)

    wanted = rows()
    wrong = [
        f"{k}: icons.rs says U+{cp:04X}, upstream {name} is "
        + (f"U+{upstream[name]:04X}" if name in upstream else "not a Material Symbols name")
        for k, cp, name in wanted
        if upstream.get(name) != cp
    ]
    if wrong:
        sys.exit("icon table disagrees with upstream:\n  " + "\n  ".join(wrong))

    font = TTFont(io.BytesIO(font_bytes))
    font = instancer.instantiateVariableFont(font, INSTANCE, inplace=True, updateFontNames=True)

    opts = subset.Options()
    opts.name_IDs = ["*"]
    opts.name_legacy = True
    opts.recalc_bounds = True
    opts.layout_features = []
    opts.glyph_names = False
    subsetter = subset.Subsetter(options=opts)
    subsetter.populate(unicodes=[cp for _, cp, _ in wanted])
    subsetter.subset(font)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    font.save(OUT)
    size = OUT.stat().st_size
    print(f"{OUT.relative_to(REPO)}: {len(wanted)} icons, {size / 1024:.1f} kB")


if __name__ == "__main__":
    main()
