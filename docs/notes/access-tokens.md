---
topic: access-tokens
triggers: [access_tokens.rs, access token, access_tokens, CARTO_TOKEN, "/api/tokens", "/tokens", resolve_access, "Credential::AccessToken", without_elevated, elevated, bot account, "midas bot setup", revoke]
updated: 2026-10-16
---

# Access tokens

What a bot or a script signs in with instead of a password. The flow they exist
for is four steps a person does by hand: sign in on cartopolis.org as the bot's
account, open **/tokens**, make one, paste it where the bot runs (`CARTO_TOKEN`,
or `midas bot setup`). The password never leaves the person who knows it, and
**Revoke** on the same page is the off switch — including for a bot that is in the
world at that moment.

The app has the same page since 2026-10-16, at Settings ▸ Access tokens
(`systems::access_tokens`), over the same routes: the list, the form with the
password asked again, and the secret shown once and held in memory only until
Done. A session token is what the app signs its HTTP calls with, which is what
`require_person` asks for, so nothing on the server changed for it.

## Why not a session

A session is the token a sign-in already hands out, and the obvious move was to
let a bot keep one. Three of its properties are wrong for a bot, and each is
right for a person, so they were not changed for everyone:

- it **slides out after 30 days unused**, and a bot left alone for a quiet month
  would find itself signed out;
- the store **evicts the oldest at 4,096**, so a bot could lose its sign-in to a
  crowd it never saw;
- **every handshake mints a new one**, so a bot reconnecting all day fills that
  ceiling and pushes real people's sessions out.

So access tokens are their own table (`access_tokens`, hashed like `sessions`)
with their own rules — no sliding TTL, never evicted, an optional fixed expiry,
50 per account — but they are resolved by the **same** `Sessions::resolve_access`
the handshake and HTTP's `Bearer` already call, so nothing that accepts a token
learned a second path. A handshake made with an access token is answered with that
token rather than a new session, and the client does not write it to `user.json`.

## What a token may do

By default, what a **player** may do, whatever the account's roles say:
`PermissionSet::without_elevated` drops the `Server` group and the "any" and
override permissions (`Permission::elevated`). A bot needs nothing more, and it
is all a stolen token buys — acting as the account in the world, not running the
server or editing other people's things. The **Elevated** box on the form opts a
token out of that, for an operator's script. The mask is applied in three places
and must stay in all three: HTTP's `access_permissions` (every route that decides
on a permission asks it), the handshake's `perms`, and the `grants` arm that
pushes role changes into a live connection — without the last, an operator
editing a role would hand a restricted token what it was made without.

Managing tokens — list, create, revoke — needs a **sign-in**, never a token, so a
stolen one can neither mint its siblings nor revoke the one its owner would use
against it. Creating one also asks for the password again, for `DeleteAccount`'s
reason: a session can be a remembered one on somebody else's device, and a token
made there outlives the device. There was a third limit planned, "may change the
account", and it was dropped because nothing on the server changes a password or
an email, and deletion already re-checks the password.

## Revoking reaches an open connection

Disabling an account gates only the *next* handshake, and that was acceptable for
a person. It is not for this: revoking is how a bot is stopped, and a bot holds
one connection for days. A connection made with an access token checks every five
seconds that the token still exists (`Sessions::access_token_live`, a hash lookup)
and closes with a 401 when it does not. Deleting or kicking the account
(`revoke_principal`) takes its tokens with it.

## Rejected: the MCP token as the bot's credential

The bot also used to need `CARTO_MCP_TOKEN`, and folding the two together was
considered. They answer different questions — an access token says *who* to the
server, the MCP token said *who may drive this running client* — and an agent
that only drives one client should not hold a credential that signs in as the
account from anywhere. The MCP token went instead, for its own reasons
([driving-the-client-from-an-ide.md](driving-the-client-from-an-ide.md)).
