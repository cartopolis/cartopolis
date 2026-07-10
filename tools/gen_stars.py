#!/usr/bin/env python3
"""Generate assets/sky/bright_stars.bin from the HYG star database.

Downloads the HYG v4.1 catalog (https://github.com/astronexus/HYG-Database,
CC BY-SA 4.0, based on the public-domain Yale Bright Star / Hipparcos data),
keeps every naked-eye star (visual magnitude <= 6.5), and writes a compact
little-endian binary the client embeds at compile time (see setup_stars in
crates/viberfox/src/main.rs):

    per star: f32 ra_rad, f32 dec_rad, f32 magnitude, f32 color_index (B-V)

Run from the repo root: python3 tools/gen_stars.py
"""

import csv
import io
import struct
import urllib.request

URL = "https://raw.githubusercontent.com/astronexus/HYG-Database/main/hyg/CURRENT/hygdata_v41.csv"
OUT = "assets/sky/bright_stars.bin"
MAG_LIMIT = 6.5

raw = urllib.request.urlopen(URL, timeout=60).read().decode("utf-8")
rows = csv.DictReader(io.StringIO(raw))

stars = []
for r in rows:
    if r.get("proper") == "Sol":  # the catalog includes the Sun; we render our own
        continue
    try:
        mag = float(r["mag"])
        ra_hours = float(r["ra"])
        dec_deg = float(r["dec"])
    except (KeyError, ValueError):
        continue
    if mag > MAG_LIMIT:
        continue
    bv = 0.5  # neutral warm-white when the color index is missing
    try:
        bv = float(r["ci"])
    except (KeyError, ValueError, TypeError):
        pass
    ra_rad = ra_hours * 15.0 * 3.14159265358979 / 180.0
    dec_rad = dec_deg * 3.14159265358979 / 180.0
    stars.append((ra_rad, dec_rad, mag, bv))

stars.sort(key=lambda s: s[2])  # brightest first (harmless, nice for debugging)
with open(OUT, "wb") as f:
    for s in stars:
        f.write(struct.pack("<4f", *s))

print(f"{len(stars)} stars -> {OUT} ({len(stars) * 16} bytes)")
