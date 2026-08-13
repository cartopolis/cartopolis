# cartopolis-admin

The account-administration GUI for a Cartopolis simulator: a small Preact app that
talks to the simulator's `/api/admin` surface (`crates/simulator/src/http.rs`).

It is deployed to <https://web.cartopolis.org> by `.forgejo/workflows/deploy-admin.yml`
on every push to `main`.

## Architecture

Borrowed from [codebrew](https://code.garage44.eu/jeroen/codebrew), which is the
house style for Garage44 front-ends — the pieces this app uses are the state
model and the application setup:

- **State is one deepsignal store** (`src/lib/store.ts`). `persistentState` is the
  shape that survives a reload (it is the localStorage whitelist, by being the
  blueprint `save()` filters against); `volatileState` is everything fetched. They
  are merged at load and exposed as a single `$s` proxy, so a component reads
  `$s.accounts` and re-renders when it changes without a hook, a selector or a
  provider.
- **`App.init` owns the bootstrap** (`src/app.ts`): build the store, then render
  `Main`. Everything shared — the store, the API client, the notifier — is
  constructed once at module scope and imported by name rather than threaded
  through props.
- **One folder per component**, `name/name.tsx` beside `name/name.css`, grouped
  into `components/pages` (a route) and `components/elements` (a piece).

## Running it

```bash
bun install
bun run dev          # http://localhost:3100, proxying /api to a local simulator
bun run build        # → dist/
bun test             # the pure logic: store merge/save, API error shaping
```

`bun run dev` proxies `/api/*` to `$CARTO_ADMIN_API` (default
`http://127.0.0.1:4748`), so point a simulator there first:

```bash
cargo run -p cartopolis_simulator -- --http-listen 127.0.0.1:4748
CARTO_ADMIN_PASSWORD=somethinglong \
  cargo run -p cartopolis_simulator -- --create-admin you
```

In production there is no Bun process: `dist/` is static files served by Caddy,
which proxies `/api/*` to the simulator on the same host. The app therefore only
ever talks to its own origin, which is also why it needs no CORS.
