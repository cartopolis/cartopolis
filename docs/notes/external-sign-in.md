---
topic: external-sign-in
triggers: [crates/simulator/src/signin, signin/http.rs, signin/providers.rs, signin/store.rs, signin/keys.rs, external_signin.rs, login_screen.rs, welcome.tsx, signin-admin.tsx, ways.tsx, linked-groups.tsx, V41__external_signin.sql, signin.key, CARTO_SIGNIN_KEY, signin_manage, "/api/auth/", "/api/account/", oauth, openid, discord, reddit, mastodon sign-in]
updated: 2026-10-24
---

# Signing in with Discord, Reddit, Mastodon, Codeberg, Google and GitHub

Design and decisions: `docs/mockups/external-sign-in/` (v3 is what was built,
with all provider configuration in the Admin UI, communities linked at launch,
all six providers meant to be on for cartopolis.org).

## What an operator has to do, and what breaks silently without it

- **Each provider needs an app registered there**, with the redirect address
  `{public_base_url}/api/auth/{id}/callback` (Admin › Sign-in shows it with a
  Copy button). Mastodon needs nothing: the server registers itself with each
  fediverse server on first use (`fediverse_signin_apps`).
- **Caddy must route `/api/auth/*` and `/api/account/*` to the simulator**
  (`@webapi` in `/data/stack/Caddyfile`, `hosted-simulator.md`). Unrouted, they
  answer the web UI's HTML with a 200, and the buttons lead to a blank page.
- **`signin.key` beside `world.db` (or `CARTO_SIGNIN_KEY`, 64 hex) seals the
  client secrets in the database.** Lose or change it and every secret fails to
  open: sign-ins with those providers fail until an operator enters the keys
  again. Back it up with the database, but not *in* it.
- A provider cannot be switched on until its keys pass a test (a real round
  trip with the operator's own account); new keys switch it off again.

## Rejected, and why

- **Checking ID tokens.** The profile is read with the access token, which came
  straight from the token endpoint over TLS (OIDC Core §3.1.3.7). That saves a
  JWT library and key-set caching, and it is the only path Discord, Reddit,
  GitHub and Mastodon offer anyway.
- **Linking an existing account on a matching verified address alone.** It
  asks that account's password once. Otherwise any provider that ever gets an
  address wrong hands over the account.
- **A typed address for Reddit and Mastodon accounts.** An unconfirmed address
  marks a pending registration, and `registration::sweep` deletes the account
  when the link expires. These accounts simply have no address.
- **The app coming back through the `/go` app link.** A link does not reach an
  Android app that is already running (`platform::android::launch_url`), so
  the app pairs by a code (`POST /api/auth/app`) and polls.
- **The sealing key in `SimConfig`**, which derives `Debug` and would print it.

## Things that are easy to get wrong

- An account made this way has `pw_hash = '!signin'` (`auth::NO_PASSWORD`).
  Wherever a password is asked again (access tokens, deleting the account,
  setting a password), such an account instead needs a sign-in through its
  provider in the last 10 minutes (`Sessions::reauthed_recently`); the API
  answers `"reauth": true` (`http::REAUTH_NEEDED`) so the page offers that.
- Round trips, tickets and pairing codes are in memory and live 10 minutes. A
  restart costs somebody mid-sign-in one more click.
- Joining through a linked group skips the community's join rule (that is what
  linking means) but not a moderator's block (`communities::join_linked`).
- The integration tests (`tests/integration/signin.rs`) run against stand-ins
  on 127.0.0.1 through `SimConfig::signin_test_base`. **As of 2026-10-24 no
  real provider has been signed in with.** Discord's and Reddit's acceptance
  of the PKCE parameters is assumed from their docs, not seen.
