---
topic: voice-agent
triggers: [crates/voice, cartopolis_voice, "mcp/voice.rs", voice_tools, "--voice-agent", voice_call, "voice agent", "Realtime API", realtime, KEEP_LISTENING, agent_voice, tap_audio, pcm_sink, "accounts.agent", "CallMember.agent"]
updated: 2026-10-23
---

# The voice agent: calls the bot takes part in by speech

Design and the decisions taken with the person steering: `docs/mockups/voice-agent/`.
Built 2026-10-23. Three pieces: `cartopolis_voice` (the bridge to OpenAI's
Realtime API, Bevy-free), `cartopolis_mcp::voice_tools` (its tools), and the
client's `systems::mcp::voice` (answering, placing, following calls).

## Why it is a device and not a service

Calls are end-to-end encrypted and the simulator must never open a frame
(`calls.md`). So the agent is a member of the call: the bot's headless client
hears through its session (`Session::tap_audio`) and speaks through it
(`Session::pcm_sink`). Both run on threads of their own, never at the app's
frame rate, which on a headless client between calls is two frames a second
(`pace_idle`, which now also keeps the full rate during any call so tool
replies are not half a second late).

## Decided while building, and why

- **The tools go through an MCP client in the same process**, not a second
  table of functions. `voice_tools::McpTools` opens an MCP session on an
  in-memory pipe to a fresh `Tools`, so names, schemas, replies and the
  client's checks are the text agent's exactly. The design proposed factoring
  out `dispatch`/`render` functions instead; the in-process client needed no
  refactor of the server and cannot drift. rmcp's `client` feature costs one
  crate (`tokio-stream`).
- **Who may make it act is decided per tool call, in the bridge**
  (`bridge::refusal`). `READ_ONLY` tools answer anybody in the call; `FULL`
  ones (moving, building, sending, `voice_call`) only while the person last
  heard above the speaking level is a configured user (`--mcp-allow`), held
  for 0.8 s between words. Building by voice needs no confirmation (the person
  steering decided so); the scope rule is the safeguard. Two people talking at
  once defeat the attribution, which is why it gates only the non-read-only tools.
- **`hang_up` and `keep_listening` are the bridge's own**, not MCP calls:
  hanging up waits until the goodbye has been played (and 400 ms after), and
  in a meeting the model calls `keep_listening` instead of answering what was
  not meant for it. Whether a realtime model reliably chooses
  `keep_listening` over speaking is **not yet measured**.
- **The outcome line of a placed call is written by the bridge**, as a
  text-only reply after the call, and posted by the client: a call to a
  stranger has no tool that writes to anybody.
- **The key is read per call** from `--voice-key-file` (else
  `OPENAI_API_KEY`), so a host can keep a short-lived Realtime client secret in
  the file and the long-lived key never reaches the node. Minting that secret
  in `midas bot` is **not built**; until it is, the key file holds the key.
- **The agent badge is an account flag** (`accounts.agent`, migration V40,
  protocol 43): `CallMember.agent`, `ContactDto.agent`,
  `ContactRequestDto.agent`. An operator sets it (`PATCH
  /api/admin/accounts/{id}` with `agent`, or "Mark as agent" on the Admin
  page). The app's call screen and the chat page's ringing dialog then say
  "AI agent · what you say goes to OpenAI" where they would say "End-to-end
  encrypted"; the contact list's second line starts "AI agent ·".

## What is verified, and what is not

- `cartopolis_voice`'s whole conversation loop against a stand-in Realtime
  server on loopback (`bridge::tests`): greeting paced at 50 frames a second,
  caller audio resampled and sent, a tool call answered, a full-scope tool
  refused while somebody untrusted speaks, an interruption truncated at what
  was played (200–1500 ms of a 2 s reply), hang-up after the goodbye, the
  outcome line.
- The in-process MCP session lists every tool and answers a call
  (`voice_tools::tests`); the session's tap and sink in a real two-session
  call through a forwarder (`session::tests`).
- **Not run against the real Realtime API**: no key was available in the
  container. The event names follow the GA API (2025-08) from memory, with the
  beta's names read too (`realtime.rs`). The first live call is the check: the
  log says `voice: connected`, then `voice: tool` per call, and `voice: the
  service refused something` for any event it did not like.
- **Not run end to end through a simulator** (two headless clients, one with
  `--voice-agent --voice-url` at a stand-in). The stand-in in `bridge::tests`
  is the starting point for that test.

## Running it

```bash
midas bot start --node <node> --name viberfox --agent --brief tools/bot/viberfox.md \
  --mcp 'cargo run -q -p cartopolis -- mcp --socket /tmp/bot/mcp.sock' \
  -- cargo run -q -p cartopolis -- --mcp-headless --mcp-listen /tmp/bot/mcp.sock \
     --connect cartopolis.org --mcp-allow jeroen --mcp-private-only \
     --voice-agent --voice-key-file ~/.config/cartopolis/voice.key
```

Then mark `viberfox` as an agent on the Admin page, so callers see the badge.
