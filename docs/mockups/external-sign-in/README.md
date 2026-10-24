---
topic: external-sign-in
triggers: [login_screen.rs, signin.tsx, register.tsx, signup.ts, "/api/register", "/api/login", AuthCredential, oauth, openid, google, github, "sign in with"]
updated: 2026-10-24
---

# Sign in with Google or GitHub: mockup

A design proposal, not code. It adds “Continue with Google / GitHub” beside the
name and password, on cartopolis.org and in the app. The account stays a
Cartopolis account with a Cartopolis name. The provider only confirms who you
are, and an account can have a provider, a password, or both.

## v3: Discord and Reddit, set up in Admin, and communities come along

![v3](v3-communities.png)

Asked for on 2026-10-24: reach existing communities, drop the garage44 button,
and configure sign-in in the backend rather than in `cartopolis.toml`.

1. The card's grid has Discord, Reddit, Mastodon, Codeberg, Google and GitHub.
   Discord gives a verified address, so it works like Google. Reddit gives none,
   so it works like Mastodon.
2. A new **Admin › People › Sign-in** tab: one row per provider with a switch,
   drag to reorder, and a status that says "working" only after a test. Stored
   in `world.db` with write-only secrets encrypted by a key from the
   environment. A community's own OpenID server can still be added here.
3. Setting one up: the provider's three steps, the redirect address to copy,
   the keys, and who may make new accounts (anyone, members of a linked server
   only, or nobody).
4. A Cartopolis community can be linked to a Discord server or subreddit by
   someone who manages it there. Whoever signs in through it is offered to
   join, on the pick-your-name screen.

Decided 2026-10-24: all provider configuration, keys included, is done in the
Admin UI and stored in the database, as drawn. Nothing goes in `cartopolis.toml`
apart from the environment key that encrypts the secrets.

Open: build section 4 with sign-in or later; which providers go on at launch
(drawn: all six).

## v2: free alternatives too

![v2](v2-free.png)

The same flow with free, non-commercial providers beside Google and GitHub:
Codeberg, Mastodon and the fediverse (any server, typed in by the person), and a
community's own OpenID server added by the operator (garage44's Forgejo drawn).
The buttons become a two-column grid, with the operator setting the order and
cartopolis.org listing the free ones first. Mastodon asks which server first and
gives no e-mail address, so the address becomes optional and the handle shows as
confirmed on the profile. Codeberg and community servers are one generic OpenID
config block each.

Open: the order (free first, or Google first), which free ones to switch on at
launch, and whether any fediverse server may sign in or only an allowed list
(drawn: any, minus `fediverse_blocklists`).

## v1

![v1](v1.png)

1. The sign-in card with the two buttons on top. They appear only when the
   server's operator has configured each provider.
2. First time: the provider's page, then one question, your name here. No
   password and no mail link, because the provider's verified address does
   what the mail link does.
3. The address already has a password account: enter that password once to
   link, rather than joining the two on the address alone.
4. The app hands off to the system browser and takes back a session token
   (`AuthCredential::Token`, so no wire change). A short code pairs the two.
   On Android the return is the existing `/go` app link.
5. Ways to sign in on the account page. The last way in can't be removed.
   Without a password, re-confirming (access tokens, delete) goes through the
   provider again.
6. Per-server configuration in `cartopolis.toml`, off by default.

Open, as listed at the bottom of v1:

- Google and GitHub only, or any OpenID server (Codeberg, Forgejo, Mastodon, a
  community's own) as well?
- Link an existing account with its password once (drawn), or automatically on
  the verified address?
- May an account have no password at all (drawn: yes)?
- Password reset by mail doesn't exist yet, and section 3 needs it.
- Import the provider's profile picture (drawn: never)?
