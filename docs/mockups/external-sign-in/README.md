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
