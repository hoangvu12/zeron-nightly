# Provider-run chats (Claude Code cloud)

A **provider-run chat** is a Zeron chat tied to one Claude Code cloud session (claude.ai/code). The session runs at Anthropic and is the record of the conversation; a Zeron device mirrors it and sends to it, so it reads and behaves like a normal chat. Terms are in `CONTEXT.md` § Hosting vocabulary; the decision and its alternatives are in ADR 0003. This is a personal, experimental feature on the `feat/provider-cloud-sessions` branch, shipped through `zeron-nightly`'s `patches-v3.toml` and off by default. It supports Claude only; names are provider-neutral so a second provider can follow.

Evidence for every protocol claim below is in the spike repo `claude-cloud-spike` (scripts `01`–`11`, redacted notes and fixtures in `fixtures/`). It was run against a real Team account on 2026-11-09, with CLI 2.1.310 locally and 2.1.319 in the cloud worker.

## 1. Scope

In v1:

- Create a provider-run chat on a project with a GitHub remote (clones its current branch) or with no project (empty sandbox).
- Every recent session of each enabled Claude account appears in the sidebar as a normal chat, whether it was started in Zeron, on claude.ai or in the Claude app.
- Live streaming, follow-ups, steering, stop, answering Claude's questions and permission prompts, model and permission-mode changes, plan mode.
- The chat works from any of the user's devices that can host it (§4).

Not in v1, recorded for later:

- Zeron's MCP tools inside the cloud session. This needs a tunnel from a device, because the relay can't be changed; it will be off by default when it comes.
- Local repos without a GitHub remote (bundle upload).
- Linux SG as an always-on preferred host (needs a Linux build of the branch).
- Hosting with no device at all, which needs a self-hosted relay.

## 2. Protocol (verified)

Base `https://api.anthropic.com`. Every request Zeron makes carries **Zeron's own identity**: `User-Agent: zeron/<version>` and no `anthropic-client-platform` header. With that identity, every operation below worked, with no Cloudflare block and no "peer origin" parking (`07-honest.mjs`). Zeron never sends the CLI's or the web app's User-Agent or platform value.

| Operation | Request |
|---|---|
| List sessions | `GET /v1/code/sessions?limit=N[&cursor=<next_cursor>]` → `{data, next_cursor}`, newest `last_event_at` first; archived sessions included with `status: "archived"` |
| Create | `POST /v1/code/sessions` with `environment_id`, `config.sources` (GitHub repo and branch, or empty), `config.model`, and the first prompt as a `user` event. **Not idempotent**: two identical creates make two sessions |
| Stream | `GET /v1/code/sessions/{id}/events/stream?from_sequence_num=N` (SSE). Stored events arrive as `client_event`, live deltas as `ephemeral_event`. It replays only stored events after N, then follows live |
| History | `GET /v1/code/sessions/{id}/events?sort_order=asc`, paged by `resume_cursor` |
| Send | `POST /v1/code/sessions/{id}/events` with `{events:[{payload}]}`. The payload `uuid` is the server's dedupe key |
| Archive | `POST /v1/code/sessions/{id}/archive` |

The payloads are Claude Code stream-json, so the existing `crates/harness/src/claude/normalize.rs` maps them. This design relies on the following observed behaviour:

- **Repeated events are ignored.** An event re-sent with the same `uuid` is acknowledged `duplicate: true` and does nothing, both 60 s and 3 min later (`06`).
- **The first answer to a prompt wins.** A second answer to the same `request_id` with a different uuid is stored and silently dropped, both when it arrives mid-turn 0.7 s after the first (`10-conflict`) and after the turn has ended (`07`). The turn completes with the first answer, and the next turn works normally.
- **Extra interrupts are harmless,** both mid-turn and while idle (`07`).
- **Two readers are fine.** Two streams on one session receive identical events, and neither is dropped (`06`).
- **Ids are stable.** `event_id` equals the payload `uuid` for every event type, and is the same across reconnects, replays, history reads and readers (`06`, `07`).
- **Turn signals.** Text streams as small deltas, and `system/init` arrives once per turn. Cloud Bash never asks for permission; MCP tools and AskUserQuestion do.

Zeron never calls the environment endpoints. Sessions are created in the account's existing cloud environment, the first `anthropic_cloud` one. If the account has none, Zeron asks the user to open claude.ai/code once.

## 3. Accounts and credentials

Each provider-run chat records the Claude account that owns its session (`accountUuid`, `accountEmail`).

- **The official CLI owns every login.**
  - The account active on the device uses `~/.claude`.
  - Any other account the user enables gets its own CLI config folder, signed in with the official flow by running `claude` with `CLAUDE_CONFIG_DIR` set to that folder.
  - The CLI keeps that folder's `.credentials.json` and `.claude.json`. It refreshes the token there under its own lock on the folder, when the token is within 5 minutes of expiry (verified in the 2.1.310 binary).
- **Zeron only reads tokens.** It reads the access token from the folder before each request and keeps it in memory only. It never writes, copies, caches, logs or refreshes a token.
  - The CLI usually writes the file atomically but can fall back to writing in place, so if the file doesn't parse, Zeron waits 200 ms and reads once more.
- **Refresh is triggered, never done by Zeron.** When the token is within 5 minutes of expiry, or a request returns 401, Zeron runs `claude auth status --json` with that folder's `CLAUDE_CONFIG_DIR`.
  - The command makes no model call and has no session side effects. It took 0.3 s, and the CLI waits up to 2 s on exit for a refresh in flight.
  - Zeron runs it with a 10 s timeout, and at most one at a time per folder (single-flight); concurrent chats wait for the same run.
  - It then re-reads the file. If the token is still expiring, or the retried request still gets a 401, the chat shows a sign-in prompt.
- **The account is checked on every read.** Each token read also reads `oauthAccount.accountUuid` from the same folder's `.claude.json` (a file read, no CLI call) and compares it with the chat's `accountUuid`. Zeron's own account switcher can replace what's in `~/.claude` at any time, so this check can't be skipped. The `auth status --json` output (`email`, `orgId`) is checked as well after each refresh.
  - A mismatch means "this device can't host this chat right now". The device stops and the chat waits for an eligible host. It never trips the kill switch.
  - The host then publishes `hostUnavailable: {deviceId, reason: "account"}` on the chat row, as a field-only `Update`, so another device can take over (§4). It clears the field once validation succeeds again; a new host clears it when it takes over. No credential or account detail beyond the reason is published.
- **Each login is independent.** Separate sign-ins have separate refresh tokens, so no two programs ever refresh the same one.
- **If the account has no folder on the device,** the device can't host the chat. When no eligible device is online, the chat shows "Sign in as <email> to continue", with a button that runs the official sign-in into a new folder.
- **If credentials move off the file, Zeron fails closed** with a clear message. Two known causes:
  - The CLI flag `tengu_windows_credman`, currently off, would move credentials to Windows Credential Manager.
  - If the CLI flag `tengu_quizzical_giraffe` stopped subcommands from refreshing, the token would expire and the chat would ask for sign-in. It would not fail silently.

## 4. Hosting and takeover

The host is the device whose engine owns the chat (`chat.deviceId`). Only the host mirrors the session and sends to it. A device is **eligible** to host a provider-run chat when its engine advertises the new device capability `provider-run-v1` (separate from the existing `CLOUD_SESSION_CAPABILITY`) and it has a login folder for the chat's account. The account check is local.

- **Default host:** the device that created or imported the chat.
- **Takeover on use.** Ownership converges; it isn't instantaneous. When the user opens or sends in the chat on an eligible device and the current host **is offline, isn't eligible, or has said it can't host**, that device claims the chat.
  - "Offline" is the relay's existing `host_offline` signal: no ping for 75 s.
  - "Not eligible" means the host's device row lacks `provider-run-v1`.
  - "Can't host" means the chat's `hostUnavailable.deviceId` is the current host. For example, its account check failed after the user switched accounts on it (§3).
  - There is no takeover while an eligible host is online, so devices can't flap, and no takeover in the background.
  - The user sees a quiet note: "Continued here, <device> was offline."
- **The claim.**
  - A new registry `Update` op writes only `deviceId`. The existing `set_chat_host` reads and re-writes the whole row, so it isn't used.
  - The op's clock is set through `RowOp.clocks` to the later of now + 5 s and one past the field's current clock, because `HlcClock::next` doesn't observe remote clocks.
  - The claimant starts work only after the relay echoes the merged row back with `deviceId` equal to itself; over the HTTPS fallback it waits for the next poll.
  - Merge is per field, newest clock wins, and the device suffix breaks ties, so all devices converge on one winner. Two claimants may both act briefly before they converge; the rules below make that overlap harmless.
- **No other writes to `deviceId`.** On this branch, every chat-row write for a provider-run chat (activity, status, title, import) is a field-only `Update` that never includes `deviceId`. A demoted host therefore can't hand hostship back with a stale write.
- **Demotion.** Every eligible engine watches its provider-run chats, using the `spawn_cutover_watcher` pattern on `watch_chats()`. When `deviceId` is no longer itself, it closes the stream, stops draining, and stops writing to that chat's transcript. Before each outbound request, the host re-checks `is_host`. Closing a stream never interrupts the session at Anthropic.
- **Failing closed.** For a chat with a provider binding, a registry read error or a missing row means "not host". Eligible engines keep a local list of provider-run chat ids for this check.
- **Forks and side chats.** The host refuses `FORK_SIDE_CHAT` and side-chat creation for chats with a `providerSession`, including requests from older clients.

**Older builds** (Linux SG and HPMEDIA today) never become host through this feature. If an older device does end up as `deviceId` (`set_chat_host` tooling, or `claim_chat` on a missing row), it can't run the chat:

- a run command carrying the `claude-code-cloud` slug fails to decode and is refused;
- a message from an older client falls back to the default harness, but every provider-run chat's `cwd` is a pseudo path (`claude-cloud://<sessionId>`) that doesn't exist on disk, so the local launch fails with "working directory … does not exist" (`crates/harness/src/executable.rs:109-124`). On Windows, the path is an invalid name rather than a missing one, so an older engine may word the error differently, but the launch still fails.

The next use on an eligible device then takes over, because the host isn't eligible.

**Commands and the overlap window:**

- **Messages, interrupts and setting changes** are sent with the Zeron command id as the event `uuid`.
  - The server ignores any repeat, so a command executed by two hosts, or retried after a crash or lost acknowledgement, reaches the session once.
  - A command whose delivery is uncertain is always retried with its **original** uuid. A new uuid is only minted when the user explicitly sends a new message.
  - Before re-sending an uncertain command, the host reads the history for that uuid and marks the command delivered if it is there.
- **Prompt answers** use a uuid derived from the prompt's `request_id` (UUIDv5), and the server keeps only the first answer. Before answering, the host checks that the `request_id` has no `control_response` in the mirrored history.
- **Creating a session** isn't idempotent, so it is never queued and **never retried automatically**.
  - The device the user is on creates it synchronously, then writes the chat row.
  - The create request carries a `tags` entry `zeron:<chatId>`. If the response is lost, Zeron pages through the session list (newest activity first, up to sessions older than the attempt) looking for that tag.
  - If it finds the tag, it adopts that session. If it doesn't, or if the server turns out not to keep custom tags (checked in milestone 1), the chat shows "Creation uncertain", with a link to claude.ai/code and a "Create again" button. Only the user can start a second create.
- **Queued messages are visible.** A message sent while no eligible device is online shows "Queued, waiting for <device or an eligible device>". If it expires (24 h command TTL), it is shown as a failed send, not dropped silently.

## 5. Data

- **`providerSession`, a new top-level chat-row field,** written once: `{provider: "claude", sessionId, accountUuid, accountEmail, orgId}`. The account check in §3 compares against `accountEmail` and `orgId`.
  - Older engines write only the fields they know (`upsert_chat`) and never touch it.
  - A copy goes into the chat document's metadata, so it survives the one case that drops unknown fields, a delete followed by a revive.
  - `harnessSessionId` is not used, because older engines would treat it as a local Claude session to resume.
- **`config.harness = "claude-code-cloud"` and `cwd = claude-cloud://<sessionId>`.**
  - If an older engine wipes `config`, any eligible engine restores it from `providerSession`.
  - A chat whose config says `claude-code-cloud` but has no `providerSession`, such as a copy made by an older client, refuses to run.
- **Chat id** is a UUIDv5 of `claude:` plus the session id. Two devices importing the same session create the same row.
- **Mirrored entries** carry their provider `uuid` and `sequence_num`.
- **Replay watermark.** `providerWatermark` in the chat document's metadata is the highest `sequence_num` whose batch, including events that map to no transcript entry, has been fully committed. Only the host advances it, and only after the batch commits.
  - The stream replays strictly after `from_sequence_num`. So a new host resumes with `from_sequence_num` set to the lower of the watermark and one less than the `sequence_num` of the oldest `control_request` that has no `control_response`. Prompts still waiting are therefore always replayed.
  - Before connecting, the host also rebuilds the list of open prompts from the mirrored transcript, so the UI shows them even before the replay arrives.
- **Deduplication.**
  - The keep-rule is the same on every device: for each provider uuid, keep the entry whose Loro element id (peer, counter) is lowest, and delete the others. Two hosts sweeping at once therefore delete the same copies and always leave one.
  - The host sweeps after each batch, on takeover, and whenever a remote transcript change arrives.
  - Clients also hide duplicates by provider uuid when they render, so a late write from a demoted host never shows twice, even before a sweep.

## 6. Sidebar and UI

- **Session list.** While the sidebar is visible, and at most once a minute, an eligible device reads each enabled account's session list from page 1. It follows `next_cursor` until it reaches sessions whose `last_event_at` is older than 7 days (the list is sorted newest-activity first).
  - **New sessions become chats through a low-clock upsert.** The registry has no conditional insert, so discovery writes the row with a fixed, old clock: the session's `created_at`, counter 0, plus this device's suffix. It sets every field's clock to that value too.
    - It creates a row that is truly missing.
    - It loses every field to any real write. A device with a stale registry, just after startup for example, therefore can't move a chat or overwrite its fields.
    - It loses to a delete's tombstone.
    - Discovery creates rows only for sessions whose `status` isn't `archived`. The relay purges tombstones after 30 days (`edge/src/registry-room.ts:42`), and deleting in Zeron archives the session, so a deleted chat stays deleted as long as its session stays archived. It comes back only if the session is resumed elsewhere, because Anthropic remains the record.
  - **Discovery writes nothing else to the registry.** The title, status (working, waiting for you, idle) and unread shown in the sidebar come from the device's latest copy of the session list, laid over the row in the UI rather than written into it. The host still records activity for a chat it is streaming, as for any chat, through field-only `Update`s.
  - **Archive and delete.**
    - **Archived on claude.ai or the phone:** when a session's `status` becomes `archived`, the host deletes the Zeron chat. A session that has merely dropped off the 7-day window is left as it is.
    - **Archived in Zeron:** affects Zeron only and leaves the session active, because an archived session can't accept new messages and Zeron chats can be unarchived and continued.
    - **Deleted in Zeron:** archives its session, so it doesn't come back through discovery. Zeron never deletes sessions at Anthropic.
    - **Revived.** If a session whose chat was deleted becomes active again on claude.ai, with activity newer than the delete, a device whose registry holds that chat's tombstone recreates the chat with a normal clock. The transcript is rebuilt from the session's history. A device that doesn't yet know about the tombstone only ever writes the low-clock upsert, which loses to the tombstone, so the stale-registry protection still holds.
- **Starting a chat.** The composer's place chip lists **Anthropic cloud** under Zeron Cloud in its Device menu, on eligible devices only. Its Project menu offers GitHub projects, "New project from GitHub…" and "No project". The model picker shows Claude models only.
- **Appearance.** Provider-run chats carry a Claude-cloud mark in place of the harness icon. They hide files, diff, terminal, background processes, fork and side chats, gated on a `remote_workspace` flag on the harness descriptor.
- **Settings.** A normal setting, "Claude cloud sessions", turns the feature on. Under it, the user picks which Claude accounts on this device show their sessions; the device's own CLI login is on by default. "Add account" runs the official `claude` sign-in into a new config folder (§3). Every enabled account's sessions appear: all non-archived sessions active in the last 7 days.
- **When the device's own login changes,** for example from daniel@ to caesar@ through Zeron's account switcher or `claude /login`:
  - The new account's sessions start appearing.
  - Chats of the previous account stay in the sidebar. This device stops hosting them (§3's account check), and another device with that account takes over when the chat is used.
  - Zeron offers "Keep daniel@'s cloud chats working on this device?", which runs "Add account" for daniel@. Nothing at Anthropic is lost either way.

## 7. Risk controls

Using a subscription login outside Anthropic's own apps and building on a private API is against Anthropic's terms, and on a Team plan a suspension could apply to the whole Team. The user has accepted this. The tests show the feature works; they say nothing about how Anthropic will enforce its terms. The design keeps exposure as low as it can:

- **Honest about who it is.** Zeron identifies as Zeron and never impersonates the CLI or the web app.
- **Logins stay with the CLI.** Sign-in and refresh happen only through the official CLI. Zeron never stores, copies or refreshes a token.
- **Only the session calls claude.ai itself makes.** No environment, settings or other undocumented endpoints.
- **Human-paced.**
  - One stream per open or running chat.
  - The session list is read at most once a minute, and only while the sidebar is visible.
  - Zeron honours `Retry-After` and backs off exponentially on 429 and 5xx.
  - It never sends anything the user didn't send.
- **Scoped failure handling.**
  - The feature turns itself off, and says why, when:
    - a `<data>/claude-cloud/DISABLED` file exists;
    - a 401 persists after one CLI refresh;
    - the list or create endpoint returns a 403 or 404 after the folder's identity has been checked;
    - an event envelope isn't recognised.
  - A 403 or 404 for one session affects only that chat: "This session no longer exists" or "This account can't open this session".
  - Nothing retries in a loop.
- **Contract tests.** The spike fixtures are replayed in the test suite. Before the patch is turned on in `patches-v3.toml`, a live smoke test runs: create, send, stream, answer a prompt, archive.

## 8. Milestones (about 4 weeks)

1. **Transport and auth (3 days):** the honest-identity client; token reads with a parse retry; `auth status` refresh with single-flight and the account check; scoped failure handling; a check that the server accepts the `zeron:` tag.
2. **Harness (4 days):** `crates/harness/src/claude_cloud/`, mapping events through `Normalizer`. Controls and prompts. Command-id and `request_id` uuids. Uncertain-delivery reconciliation. Replay tests on the fixtures.
3. **Data (3 days):** `providerSession`, deterministic chat ids, the pseudo `cwd`, provider uuids on entries, the watermark, the deterministic sweep, render-time dedupe, config repair.
4. **Hosting (4 days):** the `provider-run-v1` capability; the `deviceId`-only claim op with its clock floor and relay echo; field-only writes; the demotion watcher; failing closed; fork refusal.
5. **Sidebar and UI (4 days):** session import with paging, the place chip, the mark, panel gating, queued and failed states, settings, account sign-in.
6. **Hardening (3 days):** tests for simultaneous claims, a reconnecting demoted host, and a takeover while a prompt is unanswered, on PC and Laptop; crash-after-send and lost-create-response tests; the live smoke test.

## 9. Residual risks

- **Anthropic can change or close the private API at any time.** Scoped failure handling turns that into a clear message, not damage.
- **Anthropic can enforce its terms against the account or the Team.** The design can't measure that probability, only keep exposure low (§7).
- **The CLI can change how it stores or refreshes logins** (`tengu_windows_credman`, `tengu_quizzical_giraffe`). The feature then fails closed with a sign-in or update message.
- **Sessions are tied to their account.** A session can't be continued from a different Claude account; Anthropic enforces this. A device can host an account's chats only if it has that account's login, either as its own CLI login or as an added account. Added accounts are renewed by the CLI and occasionally need the user to sign in again.
- **Older devices on stale state.** A device kept offline for days may briefly show a stale host. It can't run the chat, and the next use on an eligible device takes over.
