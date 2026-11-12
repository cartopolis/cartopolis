---
topic: release-approval
triggers: [release approval, "self-update", Updater.java, platform/update.rs, crates/server, server-package.sh, deploy/server-package, version.json, "release key", fingerprint, BiometricPrompt]
updated: 2026-11-12
---

# Releases you approve with your fingerprint

![v2](v2.png)

**v2 (2026-11-12), proposed, nothing built.** The independent review is
Claude, asked **by the app on the owner's phone** through the Claude API (frame
4), so the only parties trusted for it are Anthropic and the phone: no server
of ours, the vps or the mini is between the question and the answer, and no
session that wrote the code. The app sends the release's code only after
checking it against the commits' git fingerprints (Forgejo is on the vps and
could serve something else), uses the review instructions built into the app
itself (approved in an earlier release, so nothing on the way can weaken them),
and the owner's own API key, kept in the phone's Keystore. Claude reports on
authenticity and hack attempts in plain language, each finding with its lines;
the fingerprint signature also covers that report, so the record says what was
approved after what review. Rejected: a reviewer on the mini (one more machine
to trust, and its report needs a key of its own to reach the phone unchanged),
and a cloud routine (no key of the owner's to sign its report with).

![v1](v1.png)

**v1 (2026-11-12), proposed, superseded by v2.** Asked for: every release of the
app (APK) and of the self-hosted server is approved by the owner personally,
with a fingerprint on their phone; a server's admin is then told and installs
it with one click; it must be simple, transparent and above all safe — and the
owner must be able to trust the code without reading all of it.

## The screens

1. **A release is waiting** — on the phone that holds the release key only.
   A session or a deploy stages a build; nothing is published.
2. **What changes, and the checks** — every change since the last approved
   release, in the words its landing was put to the owner in, and the checks a
   machine can make (below). All green is the normal case.
3. **What needs your eyes** — the parts a backdoor would need, pulled out and
   shown in full; the rest is not shown line by line.
4. **Your fingerprint signs it** — Android's own biometric sheet. The key is
   made in the Android Keystore (StrongBox where the phone has it), cannot leave
   the chip, signs only after a *strong* biometric (no PIN), and is invalidated
   when a fingerprint is added. A session driving the app through the Midas
   link can open the screen but cannot give a fingerprint.
5. **When a check fails, there is no button** — approval is off until it is
   explained, never a warning to click past.
6. **Signed and published** — the hub publishes the files with the signature.
7. **A server's admin is told** — on the Admin page of each self-hosted server,
   which has already downloaded the update and checked the signature against
   the key built into it. Nothing happens until an admin clicks **Install**.
8. **Installing** — each step shown; the new version is first run against a
   *copy* of the database; if it does not come up after the switch, the server
   puts the old version and database back by itself and says so.

## How you can trust code you have not read

No check makes reading unnecessary for certain; these make the part you must
read small, and make it impossible to swap what you approved afterwards. Each
is a machine check except the review, and a failure in any of the first two
removes the button.

| Threat | Check | Kind |
|---|---|---|
| A commit reaches `main` without a session the owner steered (a stolen push key, a stray script) | **Every commit since the last release is part of a landing the owner answered "Land it" to**, recorded by the hub with the tip it landed | blocks |
| The build machine or the VPS swaps the files after review | **Two machines build the same files** (a Mac node and vps-1, reproducible builds) and the phone downloads and hashes them itself; the signature covers those hashes | blocks |
| A dependency is poisoned | Every new or changed crate and npm package is listed with what it can do; known vulnerabilities (RustSec) | flags |
| A session writes hidden behaviour (by mistake, or steered by text it read) | The diff is scanned for what such code needs — a new network address, a process started, `unsafe`, a `build.rs`, a new Android permission, a change to sign-in, encryption or this update code — and those hunks are shown in full | flags |
| The same, subtler | Claude, asked from the owner's phone through the Claude API, reads every change for authenticity and hack attempts; it reports, it cannot approve | report |
| It behaves differently than it reads | The release ran on cartopolis.org first (it deploys after every landing): crashes, restarts, and **which addresses it talked to**, against the list it is known to use | flags |
| Lost phone | A recovery key, made once and kept offline, that can only name a new phone key, never sign a release | — |

## What the signature does not cover

The vps can always *publish*; what it cannot do is get anything *installed*:
every server and phone checks the signature against the key built into it, and
refuses an older version than the one it runs (the signed release names its
version code), so a validly signed old release with a known hole cannot be
served again. What stays as trustworthy as the vps, said plainly in the docs:

1. **What the vps serves live** — the website and web app at cartopolis.org
   (a browser fetches it on every visit and checks no signature), the hosted
   server, atlas.
2. **The first download** — the curl line and the APK page come from
   cartopolis.org; the built-in key protects from the first update on. A copy
   of the key's fingerprint and the installer's checksum somewhere off the vps
   (a mirror) would let a careful newcomer check it; only a store closes it.
3. **Withholding updates** — the vps can stop publishing; the Admin page says
   how long ago it last saw a release, so silence is visible.

## What is new for each part

- **App** (`Updater.java` beside a new `ReleaseKey.java`): the key, the review
  screen, the biometric prompt; and the updater refuses an APK without the
  owner's signature — which closes the hole `android-build.md` accepted on
  2026-11-05 (the APK signing key is on the vps).
- **Server** (`cartopolis-server`, in Rust, no shell): check
  `cartopolis.org/server/version.json` every 6 h, download, verify the
  signature, the Admin page banner and **Install**, test on a database copy,
  switch, restart, roll back. The curl installer stays one line but only places
  the program; updating is the program's.
- **midas** (the hub): stage a release instead of publishing it, record the tip
  of every landing the owner approved, run the checks, collect the change
  summaries, and publish only with a valid signature.
- **Existing installs** learn the check from the first version that carries it;
  a fresh install trusts its first download, as with any software not from a
  store.
