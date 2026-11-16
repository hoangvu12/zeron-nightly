# Your own cloud

Status: design. Decided in a grilling session on 2026-11-16; updated the same day (waking only on a
queued send, providers built into the engine, Boat as a second provider). Terms are in
[CONTEXT.md](../../CONTEXT.md) under "Cloud vocabulary". The sign-in choice is
[ADR 0003](../adr/0003-sandboxes-borrow-the-host-devices-sign-in.md); where providers live is
[ADR 0004](../adr/0004-sandbox-providers-are-built-into-the-engine.md).

A chat can run in the user's own cloud account. Two providers are built in: **Docker Sandbox** and
**Boat** (the user's own boat.dev account). It works
like Zeron Cloud ([cloud-device.md](cloud-device.md)), except that the machines and the bill are
the user's, and nothing on the edge or relay changes.

## What the user sees

1. Device picker → **Docker Sandbox** → a template picker (the account's templates, plus Blank).
2. Send. The chat starts, its sandbox is made, and the first message runs when it's ready.
3. From then on it's an ordinary chat: files, terminal, diffs, phone, all of it.

Device lists show one **Docker Sandbox** entry, never one device per sandbox.

## The shape

| | |
|---|---|
| Machines | The provider decides. Docker and Boat: **one sandbox per chat** |
| Providers | Built into the engine in Rust (`crates/engine`): Docker Sandbox and Boat (ADR 0004). Third-party providers as plugins come later |
| What runs in the sandbox | `zeron-engine headless`, a real engine, so **agents run in the sandbox** |
| How devices reach it | The relay, as for any device. No SSH |
| Who calls the provider | The **host device**: one always-on device that keeps the cloud account's key |
| Other devices | Ask the host device through the engine's existing request forwarding to a target device (`targetDeviceId`), not peer plugin calls (ADR 0004) |
| Sign-in | **Borrowed**: the host device lends short-lived access tokens (ADR 0003) |
| Server of our own | None. Revisit only if "no device of mine is on" really hurts |

### Picking the host device

Zeron picks it; Settings › Sandboxes can change it.

- **Default:** a device running `zeron headless` that's online (an always-on server). If there are
  several, the one online longest. If there are none, the device the user connected the provider on.
- **Connecting on another device** sends the provider key to the host device over the relay once.
- **Changing it** moves the key to the new host device. Running sandboxes keep going; their next
  token comes from the new one.
- If the host device is a laptop, sandbox chats stop while it sleeps. Settings shows that next to
  the picker only when it's the case.

### Hiding sandboxes

A sandbox's engine advertises a new capability (not `cloud-session`: clients route that one's
wake and sleep to Zeron Cloud, `crates/ui/src/cloud.rs:50-61`). New clients hide devices with it
and show one logical entry per provider. Old clients list each sandbox as a device and can't wake
one; everything else works for them.

## Steps

Every sandbox goes through the same steps, whatever the provider. A provider supplies only the
parts that need it; Zeron does the rest the same way everywhere and skips a step its provider
can't do.

| Step | Done by | Notes |
|---|---|---|
| Create | provider | From the picked template |
| Start | provider | |
| Install Zeron | Zeron, via the provider's *run a command* | Skipped when the template already has it |
| Sign in | Zeron | First token in; later ones over the relay (ADR 0003) |
| Secrets | provider if it has them, else Zeron | Docker: proxy secrets |
| Get the code | Zeron | `git clone`, then branch `zeron/<chat>` |
| Setup | Zeron | Step scripts, then the repo's `.zeron/setup.sh` |
| Ready | | The queued first message runs |
| Sleep | Zeron, then provider | Engine winds down (as `zeron quiesce`), then stop |
| Wake | provider, then Zeron | Start, then a fresh token |
| Delete | Zeron, then provider | Stop lending tokens, then delete |

### The provider's part

```
create(template, env)    any subset of these five
start()
stop()
delete()
run(command)

templates()              optional
save_template(name)

connect()                how the account is linked
name, icon               how it shows in the device picker and Settings
```

The icon is the provider's own mark, drawn as a glyph in Zeron's icon style (same grid, stroke and
corner treatment as the app's icons), so the picker never falls back to a generic box.

That covers places that aren't sandboxes:

| Place | Supplies |
|---|---|
| Docker Sandbox | everything |
| Boat | everything except templates; has systemd, so Install Zeron reuses Zeron Cloud's install |
| An SSH box | `run` only |
| A PC at home | `start` (Wake-on-LAN). It already runs Zeron, so Install and Sign in are skipped |

## Step scripts

The user's own scripts run before, after or instead of any step. Files are the truth: Fleet can
sync them and git can track them.

```
~/.zeron/steps/after-setup/fleet-sync.sh        every sandbox
~/.zeron/steps/docker/after-wake/git-fetch.sh   Docker sandboxes only
<repo>/.zeron/setup.sh                          that repo only
```

- The folder names the hook (`before-`, `after-`, `instead-of-`) and the step, so a moment can
  have several scripts. They run in name order.
- Where the folder sits says where it applies.
- They're stored on the host device. Each one **runs in the sandbox** when the sandbox is up for
  that step (after-start, after-setup, after-wake, before-sleep, the repo's `setup.sh`), and **on
  the host device** otherwise (before/after-create, before/after-delete). A host-side script gets
  the provider, sandbox, chat and repo as environment variables, and can ask Zeron to run a
  command in the sandbox.
- A failing script doesn't block the chat: it opens with the error and **Run again**.

### Settings › Sandboxes

One page for every provider: each connected provider with its host device, then a flat list of
the user's scripts. Steps aren't shown as a grid; a row exists only for a script that exists.

```
Docker Sandbox      Connected            Disconnect
Host device         Linux SG             ▾

Scripts                                  + Add script
fleet-sync.sh       When it's ready      All sandboxes
git-fetch.sh        When it wakes        Docker Sandbox
setup.sh            When it's ready      reportly
```

**Add script** asks two things and opens an editor that saves to the file on the host device:

| Field | Choices | File |
|---|---|---|
| When | When it's created, When it's ready, When it wakes, Before it sleeps, Before it's deleted | `after-create/`, `after-setup/`, `after-wake/`, `before-sleep/`, `before-delete/` |
| | Replace a step… (the full step list, before / after / instead) | any hook |
| For | All sandboxes, a provider, a project | `~/.zeron/steps/`, `~/.zeron/steps/<provider>/`, `<repo>/.zeron/` |

"When" also decides where the script runs, so the UI never asks.

## What's in a sandbox

- **Zeron installs only itself.** Agents, their logins and tools come from the template or step
  scripts.
- **Templates** come from the account (`ListImages`). The chat menu has **Save as template**. No
  sign-in is stored in the sandbox, and saving wipes Zeron's identity first (see Docker specifics).
- **Secrets**, GitHub included, go through the Secrets step. On Docker they're proxy secrets: the
  sandbox sees a placeholder. Elsewhere a step script provides them.
- **Previews:** none in sandboxes. Zeron's preview is WebRTC over UDP only
  (`crates/preview/src/peer.rs`), and Docker's cloud policy can't allow UDP. Docker's public port
  URLs aren't used: anyone with the link can open them.
- **Network:** each Zeron sandbox allows everything (`policy init allow-all --sandbox`). The
  account's default is untouched.

## Which agents the composer offers

| Template | Offered |
|---|---|
| Has run before | The agents it had last time (cached per template) |
| Never run | Every agent the asking device knows |

Opening a composer never creates or wakes a sandbox. If the picked agent isn't there, the chat
says "Claude Code isn't on this template" and keeps the message, so the user can switch and
resend. Zeron never swaps the agent.

## Lifecycle

| When | Then |
|---|---|
| Idle 10 min, or archived | Sleeps. Unarchiving doesn't wake it |
| Send | Wakes it (~8 s), from any device or build. See "Waking" below |
| Opening a sleeping chat | Stays asleep. Transcript shows; files, terminal and diffs offer **Wake** |
| Delete the chat | Sandbox deleted; tokens stop. The transcript stays |
| Provider time limit (Docker: 24 h) | Sleeps |
| Host device offline | Sandboxes stop themselves: the provider time limit is kept short (30 min) and the host device extends it while a chat is busy. A lent token still works until it expires (up to 1 h) |
| Sandbox deleted outside Zeron | **Start fresh**: a new sandbox; pushed work comes back, unpushed is lost |
| Engine crashes in the sandbox | Restarted by a watchdog (no systemd on Docker) |
| Disk full (Docker: 20–30 GB, fixed) | The chat says so |
| Subagent | Shares its parent's sandbox unless it asks for its own |
| New Zeron release | The sandbox's engine updates itself |

No limit on awake sandboxes; idle sleep and the time limit cover runaways.

### Waking

A send nudges the chat's host device, which is the sandbox, never the always-on host device
(`crates/client/src/live/delivery.rs:103-129`, `edge/src/chat-room.ts:877-892`). With no host in
the sandbox's DeviceRoom, the room queues the nudge and replays it to the next host that joins
(`edge/src/device-room.ts:196, 241-273`).

So while a sandbox sleeps, the **host device stands in** for it: it joins the sandbox's DeviceRoom
as host (the room checks only the user, `device-room.ts:161-165`), receives the replayed or new
nudge, wakes the sandbox **only if the chat has a queued send**, and leaves the room as the
sandbox's engine comes back. That works for every sender, the phone and old builds included, with
no edge change.

- Nudges can't be left unacknowledged: the engine dials with `nudgeAck=1`, but the edge's front
  worker rebuilds the query without it (`edge/src/device-link.ts:69-82`), so the room deletes each
  nudge as it sends it.
- Opening a chat whose host is away also nudges (rate limited). A nudge alone is not a send, so the
  stand-in checks the chat for a queued send and ignores the nudge otherwise. Opening a chat never
  wakes its sandbox.
- Losing the nudge is fine: the woken engine drains the chat's queued sends when it reconnects.

- The stand-in is the host device's own code, not a stock engine: a stock engine reconnects after
  the room closes a second host with 4409 (`crates/rpc/src/device_room.rs:389`).
- It holds one quiet socket per sleeping sandbox.
- While it holds the room, clients don't see `host_offline`; new clients show the sandbox's state
  from the host device instead.

## Boat specifics

- The user's **own** Boat account and API key, kept on the host device like Docker's key. Not Zeron
  Cloud's operator account.
- Mirrors the edge's Boat provider (`edge/src/cloud/providers/boat.ts`): REST JSON at
  `https://boat.dev/api/v1`, create with `Idempotency-Key`, stop/resume, delete with
  `X-Ascii-Confirm-Delete`, `PATCH` to extend the time limit, `POST …/commands` to run a command,
  `noEnv: true` so nothing of the account leaks in. Same state and error mapping.
- Boat machines have systemd, so Install Zeron reuses Zeron Cloud's install (the engine as a service
  that restarts and self-updates) instead of the no-systemd watchdog.

## Docker specifics

- **API, not CLI:** ConnectRPC over HTTPS, JSON bodies, the services the `sbx` CLI uses.
  Undocumented, so Docker can change it. Verified with the CLI's own token:
  - Control: `POST https://api.sandboxes-cloud.docker.com/sbx/docker.sbx.v1.<Service>/<Method>`
    (note the `/sbx/` prefix), headers `Authorization: Bearer <token>`,
    `Content-Type: application/json`, `Connect-Protocol-Version: 1`. `SandboxService/{List,Get,
    Create,Start,Stop,Delete}Sandbox`, `ImageService/ListImages`. Start and Stop return an
    operation; poll `GetSandbox` for the status (start ~0.6 s, stop 30–40 s).
  - **Run a command** (the provider's `run`): Connect server-streaming
    `POST <sandbox endpoint>/cloud.todd.v1.ProcessService/Start`, where the endpoint is
    `core.endpoint.uri` from `GetSandbox`, with the same bearer token and
    `Content-Type: application/connect+json`. Body `{args, env, workingDir, timeoutSeconds,
    includeStdout, includeStderr}`; the reply streams base64 stdout/stderr and an exit code. Runs
    as `agent` (sudo) in `/home/agent/workspace`.
  - Possibly no exec at all for Install Zeron: `CreateSandboxRequest` has `cloud.start_cmd` and
    `startup_execution.raw_image {start, ready}` (from the CLI's schemas; untested).
- **Connect:** Docker's OAuth device flow (`login.docker.com/oauth/device/code`, then
  `/oauth/token`, Docker CLI's public client id), with "paste a Docker access token" as a fallback.
  Access tokens last ~15 min and refresh tokens rotate, so the host device refreshes them like
  Zeron's own.
- **Always create with `on_timeout: stop`.** Without it the server may delete a sandbox at its time
  limit instead of stopping it.
- **Save as template wipes Zeron's identity** (its device id and data). Otherwise every sandbox from
  the template shares one device id and they kick each other off the relay.
- Stopped sandboxes aren't deleted on a schedule (none seen after 3 days; Docker's docs list none).

## Verified

- Docker stop and resume is a true suspend: processes and boot id survive, ~8 s.
- WebSocket to `edge.zeron.sh` works through Docker's proxy (101).
- Zeron 0.3.6 downloads from the edge and `zeron-engine headless` runs with no systemd, on a
  template and on a blank sandbox. Use `zeron-engine`: the release's `zeron` is the desktop app and
  needs `libasound`. (On this network Docker needs IPv4: IPv6 to Cloudflare is reset.)
- The account's templates and secrets are readable; sandboxes have no labels.
- Access tokens last 1 h (`ttl_s=3600` on every refresh in the engine log, `auth.rs:1360`).
- Nothing binds a session or token to a device: the edge reads `sid` and never uses it
  (`edge/src/auth.ts:81`); DeviceRoom and ChatRoom check only the user
  (`device-room.ts:159-168`, `chat-room.ts:164-171`).

## To verify before building

- Whether `start_cmd` or `startup_execution` can install Zeron at create time, so Install Zeron
  needs no exec (exec works today, so this is optional).

## Engine work this needs

- A **lent-token mode** for `Auth`: tokens arrive from the host device; no refresh token of its
  own (`auth.rs:589-619`).
- Lent tokens are **pinned to the sandbox's org**, not whichever org the host device has open
  (`index.ts:267, 328`).
- No step-up in a sandbox, so no plugin signing key: plugins that need one don't run there.

## Not now

- Third-party providers as plugins (ADR 0004).
- A hosted service of ours (vault, waking with no device on). Nightly is public, so it would serve
  and hold keys for every user.
- A limit on awake sandboxes.
- A vault of our own; providers' secrets or step scripts cover it.
- Previews in sandboxes. If wanted later: carry preview over the device link (TCP) when WebRTC
  fails.
