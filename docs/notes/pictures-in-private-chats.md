---
topic: chat
triggers: [pictures, "/api/pictures", chat_pictures, "Payload::Picture", seal_attachment, snapshot, polaroid, send_picture, see_picture, "picture store"]
updated: 2026-10-01
---

# Pictures in private chats

A person or a bot can send a picture into a private chat: a photo or file from
the device, a **snapshot** of the map as they see it, or (a bot) what its avatar
sees. `systems::chat_pictures` on the client, `simulator::pictures` on the
server, `cartopolis_e2ee::{seal_attachment, open_attachment}` between them.

## The server never sees one

A private chat is end to end encrypted, and its log refuses any entry over
64 KB on purpose (`e2ee::MAX_MESSAGE_BYTES`) so that it cannot become a file
store. So a picture travels in two halves:

1. The sending device shrinks it to the place-photo size
   (`utils::photo::prepare`, a 1600 px JPEG), encrypts it with
   ChaCha20-Poly1305 under a **random key made for that picture alone**, and
   uploads the ciphertext to `POST /api/pictures`, which names it by its hash.
2. Only then does it send `Payload::Picture { hash, key, nonce, … }` into the
   chat. That message is sealed like any other, so the key exists only inside
   something just the chat's members can open.

The server stores ciphertext it has no key for, so it checks nothing but the
size (`picture_limits::MAX_PICTURE_BYTES`, 4 MB) and a per-account rate
(`MAX_PICTURES_PER_HOUR`, 120, in memory). It keeps no database row: a picture
belongs to a conversation the server cannot see into, so there is nothing about
it worth recording. Fetching one needs a session **and** the 256-bit hash, which
in practice only a chat member has read out of a sealed message, and what it buys
anybody else is ciphertext. The client's disk cache holds the same ciphertext
(`pictures/{hash}.bin` in the `Cache` namespace), never the opened JPEG.

The AEAD comes from the RustCrypto provider MLS already runs on
(`OpenMlsRustCrypto`), so this added no dependency and no C — the constraint the
whole e2ee crate was chosen under.

**No protocol revision.** `Payload` is append-only and `Picture` went on the end
(postcard index 5, which a test pins). An older app shows "a message this version
can't display", which is the designed behaviour for an unknown kind.

## What was left out

- **Public chat pictures.** That would be a wire change to `ChatSend`, and the
  harder problem is moderation: an image posted to everyone near you is the
  user-generated-content case a store review asks about, with a report path
  that would have to carry pictures the server has to look at. Private chats
  are between people who accepted each other as contacts.
- **Retention.** Nothing deletes a stored picture. The store is content
  addressed and holds only ciphertext; a sweep would need to know which
  pictures no chat still names, which the server by design cannot.

## How it looks

Every chat picture is drawn as a **Polaroid**: warm white paper, a narrow edge
round the photo and a deep one at the bottom where the caption is written, and a
soft shadow (`chats::bubble`). The same card whoever sent it, so a picture reads
as a photograph handed over rather than a message in somebody's colour. Tapping
it opens it full size in the same viewer window as a place's photo.

A **snapshot** reuses the dev panel's screenshot path (`ui::drive_screenshots`,
`ScreenshotState::chat_snapshot`): egui is suppressed for the one frame that is
captured, so the picture is the map without the panels, and the pixels go to
`chat_pictures` raw — encoding a whole window on the main thread would stall the
frame, and the upload task re-encodes on a worker anyway.

## The bot

`send_picture` sends what the avatar sees through its own eyes (the same
first-person capture as `look`, at full resolution from the PNG it writes) or a
file. `see_picture` returns a picture somebody sent, as an image block, subject
to the same allow-list as chat: a picture from somebody the agent may not hear is
withheld, because text in an image is an instruction channel like any other.
Verified 2026-10-01 between two headless clients on a local simulator: request,
accept, `send_picture`, `read_chat` on the other side, `see_picture` returned the
image the first one took.
