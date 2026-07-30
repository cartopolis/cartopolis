---
topic: geocoding
triggers: [geo_nav.rs, nominatim, platform/http.rs, "search bar", "slow lookup"]
updated: 2026-07-30
---

# Why a Nominatim lookup is slow, and where the time actually goes

The self-hosted instance answers a *warm* query in **10–60 ms** and a *cold* one
in **4–10 s**. 10 s is Nominatim's own `NOMINATIM_QUERY_TIMEOUT`, and it does not
fail quietly: the endpoint returns **HTTP 503 with the plain-text body `Query took
too long to process.`**, which `ureq` raises as an error, so the search bar shows
a ten-second spinner and then "Search failed: …". A city name is almost always
warm; a *street + city* ("Beatrixlaan Rijswijk") is the shape that goes cold.

## It is storage latency, not the query

Measured 2026-07-30 on `garage44.eu`, `dd … iflag=direct`, depth 1:

| | 8 KB reads | sequential |
|---|---|---|
| `/` — `sda`, local NVMe | 67 MB/s (~8,000 IOPS) | 807 MB/s |
| `/data` — `sdb`, OVH **network** block volume | **1.9 MB/s (~240 IOPS)** | 44 MB/s |

A Nominatim search bitmap-scans `search_name` and touches ~1,000–5,000 random
8 KB pages. The *same statement*, same parameters, ran **4.1 s cold and 15 ms warm
with `Buffers: shared hit=4919, read=0`** — 4,900 random reads at ~0.8–3.4 ms each
is the whole of the difference. So the fix is to keep the working set off that
volume and in RAM, which is why the PGDATA is a bind mount on the root NVMe
(`/srv/nominatim/pgdata`) and `shared_buffers` is 8 GB rather than the image's 2 GB.
Docker's data-root is still `/data/docker`, so a *named* volume cannot land on the
fast disk — it has to be a bind mount.

## Things that look like the cause and are not

- **Word frequencies.** `word.info->>'count'` is populated; `nominatim refresh
  --word-counts` is not the missing step.
- **Missing indexes.** All of `idx_search_name_{name,nameaddress}_vector`,
  `idx_placex_*` are present and the plan uses them.
- **The `coalesce(null, …)` in the emitted SQL.** That is deliberate — Nominatim's
  `no_index()` (`db_search_lookups.py`, `Restrict`) suppresses the index on the
  *secondary* condition on purpose so the primary GIN lookup drives the scan.
  It does defeat the index (`COALESCE(x)` with one argument is not folded to a
  bare `Var`, so no GIN index matches), and wrapping *both* conditions turns the
  query into a 61 s parallel seq scan — but Nominatim only ever wraps one.
- **`plan_cache_mode = force_custom_plan`.** No measurable effect; reverted.
- **`max_parallel_workers_per_gather = 0`.** Nominatim's docs recommend it for
  query instances; here it made things *worse*. Left at 2.

`jit = off` (also upstream's recommendation) is set via `ALTER DATABASE` and kept.

## The client's own budget

`geo_nav.rs` adds 120 ms of debounce plus a 150 ms floor between requests, and
holds **one request in flight** — so a single slow reply stalls every later
keystroke behind it. That is deliberate and small next to the server.

What was *not* deliberate: `ureq::get()` builds a fresh `Agent`, and therefore an
empty connection pool, on every call. Each lookup paid a full TCP + TLS handshake —
`conn=0.041 tls=0.094 ttfb=0.162` against `nominatim.garage44.eu`, i.e. **~90 ms of
a ~160 ms request**. `platform::http` now holds one process-wide pooled agent.

Unrelated to speed, and worth knowing before "the search is broken" gets filed:
Nominatim does no prefix matching to speak of. `Gron` and `Groning` return `[]`;
results start at `Groninge`.
