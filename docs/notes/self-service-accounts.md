---
topic: self-service-accounts
triggers: [registration.rs, houses.rs, mail.rs, "register", "sign up", "verification", "verify email", "smtp", "claim a house", "nummeraanduiding", "Locatieserver", "PDOK", V11__account_email, V12__house_claims]
updated: 2026-08-29
---

# Strangers can make accounts now, and claim one house each

Two facts the code cannot state on its own: which registers this is joined to and
why, and which of the obvious simpler designs were tried and lost.

## The address join takes two services, and neither one does the job alone

Claiming a house means turning what somebody typed into a building the client
already draws. Both halves are PDOK's, both are keyless, and the split is not
negotiable:

| Service | Answers | Does **not** carry |
|---|---|---|
| Locatieserver (`api.pdok.nl/bzk/locatieserver/search/v3_1/free`) | free text → addresses, with `nummeraanduiding_id` + `adresseerbaarobject_id` | the building |
| BAG WFS (`service.pdok.nl/lv/bag/wfs/v2_0`, `bag:verblijfsobject`) | unit → `pandidentificatie` | any tolerance for typed text |

`pandidentificatie` is the same string the LoD2.2 extractor writes into the
tiles and `systems::building_facts` reads back, so the join to the geometry is an
exact string match and never a spatial test.

Two things about those services cost a debugging session each, measured
2026-08-29:

- **The BAG WFS accepts `CQL_FILTER` and ignores it.** No error, no warning: it
  answers with the first page of the whole layer, so a lookup for a Groningen
  address returned a building in Appingedam and looked entirely plausible. The
  filter that works is FES 2.0 XML in the `filter` parameter.
- **`gemeentenaam:"Groningen"` also matches Midden-Groningen.** The Solr field is
  tokenised, so the quoted phrase is not an exact match, and a neighbouring
  municipality forty kilometres away comes back looking local. The `fq` is kept
  as a *narrowing* filter and the rule is an exact comparison in Rust afterwards,
  over-fetching four times the page so the filter does not empty it.

## Rejected: keying a claim on the building

The obvious design — one `pand`, one owner — is wrong in the only city this is
switched on for. A `pand` in a centre is a whole block of flats, so the first
caller would take everybody else's front door with their own. The key is the
**nummeraanduiding** (the address); the `pand` rides along and is deliberately
not unique.

## Rejected: a `verified` column with a default

Every account that existed before this had no email address at all — they are
made by `--create-admin` and the admin API. Defaulting them to unverified locks
out the running server; defaulting them to verified makes the flag mean "verified
or ancient". The gate is stated over two columns instead, and reads the same for
a row of either vintage:

    pending  ==  email IS NOT NULL AND email_verified_at IS NULL

An operator-made account has no email and is therefore never pending. Nothing
about an existing database changes.

`disabled` is deliberately *not* reused for this. It is the operator's switch,
and if verification cleared it then confirming an email would silently re-enable
an account an admin had turned off.

## Rejected: sending mail from the VPS

There is no MX for cartopolis.org and no mail server in the stack, and a VPS
sending directly needs SPF, DKIM, DMARC, a PTR record and a reputation nobody
here maintains. The failure mode is the bad one: the far side accepts the mail
and files it as spam, so registration looks broken to the person and healthy from
the server. So `mail::Mailer` speaks SMTP to somebody else's relay
(`CARTO_SMTP_URL`) and has no opinion about which.

**With no relay configured it logs each message, link included, and reports
success.** That is the dev loop and what the test suite runs against — a
registration flow that needs a live relay to exercise is one nobody exercises.
It warns at startup and at every send, because a deployment in that state is
writing account-activation links into its own journal.

## What a deployment has to set

`--http-listen` is still the opt-in; registration and claiming are on by default
once it is up.

| Env | What happens without it |
|---|---|
| `CARTO_SMTP_URL` | links go to the log, not the mailbox |
| `CARTO_MAIL_FROM` | sends as `cartopolis@localhost`, which most relays refuse |
| `CARTO_PUBLIC_BASE_URL` | links point at `http://localhost:3100` |
| `CARTO_REGISTRATION_OPEN` | open |
| `CARTO_HOUSE_CLAIMS_OPEN` | open |
| `CARTO_CLAIM_MUNICIPALITY` | Groningen |

`public_base_url` is **never** taken from the request's `Host` header. That
header is attacker-controlled, and a verification link built from it is a link an
attacker chose the destination of, mailed to a real person over this server's
reputation.

## Not built, and deliberately

- **The claim is a row, not yet a thing in the world.** Nothing in `cartopolis`
  reads `house_claims`; drawing the held house, letting the owner in, or naming
  it on the map is separate work. The join key (`pand_id`) is stored so that work
  needs no migration.
- **No proof of residence.** First come first served, with an operator list
  (`/api/admin/claims`) and a release button as the only dispute mechanism. The
  alternative considered was holding a home address per account to compare
  against, which is more personal data than the account store carries and is not
  proof either.
- **No password reset.** Registration mails a link; forgetting a password still
  needs an operator. The pieces (a token table, a mailer, a throttle) are now all
  present, so it is a small feature rather than a project.
