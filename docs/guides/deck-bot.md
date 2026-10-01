# The agent-driven account on the Steam Deck

`viberfox` is an ordinary account, signed in from a headless client on the
Steam Deck and driven over MCP by an agent. The agent can walk the account's
avatar, read chat from an allow-list, reply, and build and script objects
(`systems::dev::mcp::agent`). Why it is shaped this way is in
[driving-the-client-from-an-ide.md](../notes/driving-the-client-from-an-ide.md);
this page is what runs where and how to change it.

```
agent container on the VPS ──http──▶ 172.19.0.1:4750  (VPS, agent Docker network gateway)
                                        │  viberbot-mcp-tunnel.service: ssh -L, a key that can
                                        │  open this one port and nothing else
                                        ▼
                              Deck, podman container `arch`
                              127.0.0.1:4760  viberbot.service, user viberbot
                              cartopolis --mcp-headless --connect cartopolis.org
```

An agent environment that drives the bot needs **the URL and the token and
nothing else**: no ssh key to the VPS or the Deck.

## The agent that plays it: `viberfox-bot`

A second Remote Control service in `/data/agent/docker-compose.yml`, beside
`viberfox-agent`, shown in claude.ai/code as **viberfox-bot**. It reads what
strangers write in chat, so it holds as little as possible:

| What | Where |
|---|---|
| Its own claude.ai login | the `agent_bot-claude` volume, and no other volume of the other agent's |
| Its instructions, MCP server and permissions | `/data/agent/bot/` (root-owned, mounted read-only): `CLAUDE.md`, `.mcp.json`, `.claude/settings.json` |
| The MCP token | `/data/agent/bot.env` (0600), read by the `.mcp.json` header as `${VIBERFOX_MCP_TOKEN}` |
| Tool-call timeout | `"timeout": 660000` in `.mcp.json`. Without it Claude Code cuts every call at 60 s, and a waiting `read_chat` never returns |
| Network | `agent_bot`, its own. It reaches the tunnel and the internet, not the other agent containers |

The session runs in `dontAsk` mode with only `mcp__viberfox` allowed. The shell,
file reading and writing, the web and subagents are denied. Because the
directory is read-only and root-owned, it cannot widen its own permissions.

A new login (only if the volume is lost) is a manual step: `claude auth login`
and the first-run trust and Remote Control prompts in
`docker compose run --rm -it --entrypoint bash viberfox-bot`, then
`docker compose up -d viberfox-bot`. Doing it from an agent session is refused
by the permission check as creating an agent, and that refusal is correct.

## On the Deck (inside the `arch` container)

The Deck's ssh endpoint (`ssh deck`, port 2222) is a **rootless podman
container**. Its root cannot change the Deck's own firewall, which is why the MCP
port is kept on loopback and reached through a tunnel instead of being opened
on the WireGuard address.

| What | Where |
|---|---|
| The account's own Unix user | `viberbot` (no password, no login shell) |
| The binary and its assets | `/home/viberbot/cartopolis/{cartopolis,assets/}`, found through `CARTO_ASSET_ROOT` |
| Secrets | `/home/viberbot/bot.env` (mode 600): `CARTO_USER`, `CARTO_PASSWORD`, `CARTO_MCP_TOKEN` |
| The service | `/etc/systemd/system/viberbot.service` |
| The tunnel's key | one line in `/home/deck/.ssh/authorized_keys`, marked `viberbot-mcp-tunnel` |

The service listens on `127.0.0.1:4760`, not the default 4750, because other
agents run clients on the Deck and 4750 is theirs. `--mcp-allow jeroen` is the
allow-list. Change it in the unit and `systemctl restart viberbot`.

**On and off.** The service is **enabled** (since 2026-10-01; it was off from
2026-09-27), so it starts at boot and restarts itself if it crashes. Idle it
costs about 0.1 of a core and 6 % of the GPU. `ssh deck 'sudo systemctl stop
viberbot'` takes it down, `disable` keeps it down across a reboot, and `start`
brings it back; it signs in at launch. Its `sign_out` and `sign_in` tools take the avatar out of the world
and back without stopping it. `stop` ends the process.

**Updating the client** means copying a new binary and the `assets/` it was
built with, together. The binary reads shaders and models from `assets/` at run
time, so a new binary over old assets can fail to draw or crash on a shader it
no longer matches.

It is built in **`/home/deck/bot-build`**, a checkout of its own, not in the
`deck-1` build slot. A slot is a shared checkout that another agent may switch
to its own branch at any moment. That happened while this was being set up:
the "clean build" and the passing tests turned out to be another branch's.

```bash
ssh deck 'cd ~/bot-build && git fetch -q origin main && git checkout -q --detach FETCH_HEAD && git lfs checkout \
  && PATH=$HOME/.cargo/bin:$PATH nice cargo build --locked -p cartopolis \
  && sudo install -o viberbot -g viberbot -m 755 target/debug/cartopolis /home/viberbot/cartopolis/cartopolis \
  && sudo rsync -a --delete --chown=viberbot:viberbot assets/ /home/viberbot/cartopolis/assets/ \
  && sudo systemctl restart viberbot'
```

Logs: `ssh deck 'sudo journalctl -u viberbot -n 100'`.

## On the VPS

| What | Where |
|---|---|
| The tunnel | `/etc/systemd/system/viberbot-mcp-tunnel.service` |
| Its key and the Deck's host key | `/etc/viberbot/` (root, mode 700) |

The tunnel publishes on `172.19.0.1`, the gateway of the `agent_default` Docker
network. That address is reachable from containers on that network and not
from outside the box. The unit restarts every 30 s while the Deck sleeps.

## Undoing it

```bash
ssh host 'cd /data/agent && docker compose rm -sf viberfox-bot && docker volume rm agent_bot-claude \
  && cp docker-compose.yml.bak-20260927-bot docker-compose.yml && sudo rm -r bot bot.env'
ssh host 'sudo systemctl disable --now viberbot-mcp-tunnel && sudo rm -r /etc/systemd/system/viberbot-mcp-tunnel.service /etc/viberbot && sudo systemctl daemon-reload'
ssh deck 'sudo systemctl disable --now viberbot && sudo rm /etc/systemd/system/viberbot.service && sudo systemctl daemon-reload \
  && sed -i "/viberbot-mcp-tunnel/d" ~/.ssh/authorized_keys && sudo userdel -r viberbot && rm -rf ~/bot-build'
```

The account itself lives on the simulator. Disable or delete it in the admin
pages (`https://cartopolis.org/admin`).
