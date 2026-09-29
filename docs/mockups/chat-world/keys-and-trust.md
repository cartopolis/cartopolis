# Keys and backup: decisions for encrypted chat

Decided 2026-09-26, alongside the v5 mockup and its "who can read it" addendum.
Covers private chats only (direct messages and private groups). Here, Global and
places you follow are public and not encrypted.

## The decisions

1. **History on a new device: an encrypted backup, unlocked with a recovery key.**
2. **The web client: encrypted like the apps, with a plain note** that a browser
   runs whatever code the server sends it.
3. **Identity proofs (email, fediverse profile) are left out for now.** They were
   designed and then taken out of scope the same day. Nothing below depends on
   them, and they can be added later on top of the account key.

## One account key

Each account gets one **account key** in addition to a key per device. It does two jobs:

- **It vouches for your devices.** A new phone is trusted because the account key
  signs it, not because the server says so.
- **It lives in the backup.** The backup is locked with the recovery key, so a
  new phone plus the recovery key gets the account key and your history back.

It is also where identity proofs or in-person verification would attach later.

Setting up a new phone:

1. Sign in.
2. Enter the recovery key.
3. The phone unlocks the backup, gets the account key and signs itself.
4. Your contacts see the new device as yours, with no warning.

Without the recovery key, a new phone still works, but:

- it starts with an empty history
- contacts see "new key" for you

## The recovery key

- It is created when the first private chat is set up and shown once: 12 words,
  with a prompt to store it in a password manager or on paper.
- **If you lose it and all your devices, your history is gone.** Nobody can
  recover it, including the operator. The setup screen says so plainly.
- It can be replaced from any signed-in device. The old one stops working.

## The backup

- Your phone keeps an encrypted copy of your messages on the server and adds to it
  as messages arrive.
- It is locked with a key that only the recovery key opens.
- The server stores it but cannot read it.
- Pictures in points are included in the same way.

## When a key changes

If a contact's key changes without their account key signing the new one, every
chat with that person shows a warning. With no proofs yet, this warning and the
device list are the only defence against a swapped key. That is a known limit of
leaving proofs out.

## The web client

The web client is encrypted the same way as the apps. The security page says:

> In the browser, the code that encrypts your messages is downloaded from the
> server each time. That protects your messages from anyone reading the server's
> storage, but not from a server that has been changed to send different code.
> The apps don't have this weakness.

## Still open

- **Which library**: MLS through `openmls` with its pure-Rust crypto, because the
  Android and web builds have no C compiler. Not yet checked that it builds for
  either.
