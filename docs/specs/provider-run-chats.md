# Spec: Provider-run chats (Claude Code cloud)

Label: `ready-for-agent`

## Problem Statement

My Zeron chats only run on my own devices. When my PC sleeps or I close the laptop, the agent stops, and the work only continues when that device is back. Anthropic already runs Claude Code sessions in its own cloud (claude.ai/code). They keep working with every device off, are paid for by my Claude plan, and I can check on them from my phone. But they live in a separate app. I can't see or drive them from Zeron next to my other chats, and sessions I start on claude.ai or my phone never show up in Zeron.

Zeron Cloud doesn't solve this for me. It runs the Zeron team's build on Zeron's machines, so my personal nightly can't extend it, and its sessions don't appear on claude.ai or in the Claude app.

## Solution

Zeron gets a new kind of chat, the **provider-run chat**: a normal Zeron chat tied to one **provider cloud session**. In v1 the only provider cloud is Anthropic's Claude Code cloud.

- **Starting one.** I pick "Anthropic cloud" as the place in the composer, choose a GitHub project or "No project", and send. The session runs at Anthropic.
- **One chat, many places.** Every recent session of each Claude account I enable appears in my sidebar as a normal chat, wherever I started it. I can stream, send, steer, stop, answer Claude's questions and approve its prompts, as in a local chat.
- **Any device can drive it.** One of my devices acts as the chat's **host** and mirrors the session. If that device is offline, or can't host it, the device I'm using takes over when I open or send to the chat.
- **The session is the record.** Anthropic's session is the source of truth; the Zeron chat mirrors it.
- **What's different from a local chat.** The code lives in a **remote workspace** at Anthropic, so files, diff, terminal and forks don't apply.
- **Logins and identity.** Zeron identifies itself honestly, never handles my logins beyond reading the token, and leaves sign-in and renewal to the official `claude` CLI.

## User Stories

### Starting a chat

1. As a Zeron user, I want "Anthropic cloud" in the composer's place chip, under Zeron Cloud, so that I can start a chat that keeps running with my devices off.
2. As a Zeron user, I want to start an Anthropic-cloud chat on a project with a GitHub remote, so that Claude works on my repo's current branch.
3. As a Zeron user, I want to start an Anthropic-cloud chat with "No project", so that I get an empty sandbox for quick tasks.
4. As a Zeron user, I want "New project from GitHub…" to work with Anthropic cloud, so that I can use a repo I haven't cloned on any device.
5. As a Zeron user, I want the model picker to show only Claude models when Anthropic cloud is selected, so that I can't pick a model the session can't run.
6. As a Zeron user, I want "Anthropic cloud" offered only on devices that can host it, so that I never start a chat that can't run.

### Seeing sessions

7. As a Zeron user, I want every session I started on claude.ai or the Claude app in the last 7 days to appear in my sidebar, so that all my cloud work is in one place.
8. As a Zeron user, I want sessions grouped under their matching project, and repo-less ones under "Chats", so that they sit next to my local work.
9. As a Zeron user, I want a session that gets new activity to appear even if it's older than 7 days, so that resumed work isn't hidden.
10. As a Zeron user, I want each provider-run chat to show working, waiting-for-me or idle, and unread, so that I can tell at a glance what needs me.
11. As a Zeron user, I want a Claude-cloud mark on these chats, so that I can tell them apart from local chats.
12. As a Zeron user, I want search, filters, pins and grouping to work on these chats like on any other chat, so that nothing about them is special to manage.
13. As a Zeron user with several Claude accounts, I want sessions from every account I've enabled, so that switching accounts doesn't hide work.

### Working in a chat

14. As a Zeron user, I want replies to stream live word by word, so that the chat feels like a local one.
15. As a Zeron user, I want tool calls shown the way Zeron shows them for local Claude Code chats, so that I can follow what Claude is doing.
16. As a Zeron user, I want to send follow-ups and steer while Claude is working, so that I can redirect it without waiting.
17. As a Zeron user, I want the stop button to interrupt the session, so that I can halt a turn that's going wrong.
18. As a Zeron user, I want to answer Claude's questions in Zeron, so that a session waiting on me doesn't stall.
19. As a Zeron user, I want to approve or deny permission prompts in Zeron, so that I stay in control of tool use.
20. As a Zeron user, I want to change the model, permission mode and plan mode, so that I have the same controls as a local chat.
21. As a Zeron user, I want messages I typed on claude.ai or my phone to appear in the Zeron chat, so that the conversation is complete wherever I read it.
22. As a Zeron user, I want the chat to catch up on everything that happened while my devices were off, so that I never miss what Claude did.
23. As a Zeron user, I want other Zeron chats to be able to message a provider-run chat, so that it works with delegation like any chat.
24. As a Zeron user, I want files, diff, terminal, background processes, fork and side chats hidden on these chats, so that I'm not offered things that can't work.

### Devices and hosting

25. As a Zeron user, I want to open a provider-run chat on my Laptop while my PC is off and keep working, so that my work isn't tied to one device.
26. As a Zeron user, I want a quiet note like "Continued here, PC was offline" when a device takes over, so that I know what happened.
27. As a Zeron user, I want chats never to move between my devices on their own, so that I'm never surprised by where a chat runs.
28. As a Zeron user, I want each message I send to reach the session exactly once, even if two devices briefly both act as host or one crashes mid-send, so that Claude never gets duplicate instructions.
29. As a Zeron user, I want my answer to a question to count once, and the first answer to win, so that a stale device can't override me.
30. As a Zeron user, I want "Queued, waiting for an eligible device" when no device can host the chat right now, so that I know my message hasn't been sent yet.
31. As a Zeron user, I want a queued message that expires to show as failed, so that it never disappears silently.
32. As a Zeron user, I want a device whose Claude login no longer matches a chat to hand the chat over to a device that does have it, so that switching accounts on one device doesn't strand my chats.
33. As the host device, I want to stop streaming and sending the moment another device becomes host, so that two devices don't keep mirroring the same session.
34. As the host device, I want to restore a provider-run chat's settings if an older Zeron build wiped them, so that the chat keeps working.
35. As a Zeron user with older Zeron builds on some devices, I want those devices to show provider-run chats without ever running them, so that nothing runs in the wrong place.

### Creating safely

36. As a Zeron user, I want a session created only once, even if the network drops during creation, so that I don't end up with duplicate sessions doing the same work.
37. As a Zeron user, I want "Creation uncertain" with a link to claude.ai and a "Create again" button when Zeron can't tell whether a session was created, so that I decide whether to retry.

### Archive and delete

38. As a Zeron user, I want archiving in Zeron to affect only Zeron, so that I can unarchive and continue the session later.
39. As a Zeron user, I want a session I archive on claude.ai or my phone to disappear from Zeron, so that my sidebar matches what I've finished.
40. As a Zeron user, I want deleting a chat in Zeron to archive its session, so that it doesn't come back in my sidebar.
41. As a Zeron user, I want a deleted chat to come back with its history if I resume its session on claude.ai, so that Anthropic stays the record.
42. As a Zeron user, I want unarchiving a chat whose session was archived on claude.ai to tell me so, with a link, so that I know why it can't take messages.

### Accounts and settings

43. As a Zeron user, I want a normal setting, "Claude cloud sessions", to turn the feature on, so that it's easy to find and off until I want it.
44. As a Zeron user, I want my device's own Claude login enabled by default, so that the common case needs no setup.
45. As a Zeron user, I want "Add account" to run the official Claude sign-in in my browser, so that I can see another account's sessions on this device.
46. As a Zeron user, I want Zeron to offer to keep my previous account's chats working when I switch the device's Claude login, so that switching costs one click.
47. As a Zeron user, I want "Sign in as <email> to continue" when no device has a chat's login, so that I know how to unblock it.
48. As a Zeron user, I want Zeron never to store, copy or renew my Claude tokens itself, so that my logins stay with the official CLI.
49. As a Zeron user, I want an expired login to show "Sign in again" instead of failing silently, so that I can fix it in one step.

### Risk and failure

50. As a Zeron user, I want Zeron to identify itself as Zeron to Anthropic and never impersonate the CLI or the website, so that I'm not adding deception to an already unsupported use.
51. As a Zeron user, I want Zeron to read the session list at most once a minute and only while the sidebar is visible, so that its traffic stays human-paced.
52. As a Zeron user, I want the feature to switch itself off with a clear reason if Anthropic changes or blocks the API, so that it fails safely instead of misbehaving.
53. As a Zeron user, I want a single missing or forbidden session to affect only that chat, so that one stale session doesn't disable everything.
54. As a Zeron user, I want to disable the feature completely by placing a file, so that I can turn it off instantly without a rebuild.
55. As the maintainer of my nightly, I want the feature to ship as a disabled entry in the v3 patch list, which I enable after a live smoke test passes, so that a broken build never reaches my devices.

## Implementation Decisions

**Vocabulary and ADR.** The terms are host, Zeron Cloud, provider cloud, provider cloud session, provider-run chat and remote workspace, as in the project glossary. ADR 0003 records why: no documented Anthropic interface can carry a live chat, the relay and Zeron Cloud run the Zeron team's builds, and the terms-of-service risk is accepted on the user's account and Team. The full design lives with the ADR.

**New harness.** Add a harness with the slug `claude-code-cloud`, implemented natively over HTTP and SSE, as the opencode harness already does. Its payloads are Claude Code stream-json, so it reuses the existing Claude stream-json normalizer to produce agent events. The harness descriptor gains a `remote_workspace` flag, which turns off files, diff, terminal, background processes, fork and side chats in the UI. Each provider-run chat stores `cwd` as the pseudo path `claude-cloud://<sessionId>`, so any accidental local launch fails because the folder doesn't exist.

**Provider API contract.** Verified live on 2026-11-09:

| Operation | Contract |
|---|---|
| List | `GET /v1/code/sessions?limit&cursor` → `{data, next_cursor}`, newest `last_event_at` first; archived sessions included with `status: archived` |
| Create | `POST /v1/code/sessions` with an environment id, GitHub source or none, model, and the first prompt as a `user` event. Not idempotent |
| Stream | `GET …/events/stream?from_sequence_num=N`, an SSE stream of stored `client_event`s and live `ephemeral_event` deltas. It replays strictly after N, then follows live |
| History | `GET …/events?sort_order=asc`, paged by `resume_cursor` |
| Send | `POST …/events`. The payload `uuid` is deduped by the server; a repeat is acknowledged `duplicate: true` |
| Archive | `POST …/archive` |

Observed semantics the design relies on:
- The first answer to a `request_id` wins; later answers are dropped silently.
- Interrupts are idempotent.
- `event_id` equals the payload `uuid` and is stable.
- Requests carry `User-Agent: zeron/<version>` and no client-platform header, and every operation works that way.
- No environment or settings endpoints are called.

**Credentials module.**
- **Folders.** Each enabled account maps to a CLI config folder. The device's own login is `~/.claude`; added accounts get their own folder, signed in by running `claude` with `CLAUDE_CONFIG_DIR` set.
- **Reading.** The token is read before each request, held in memory only, and re-read once after 200 ms if the file doesn't parse.
- **Identity.** Every read also checks `oauthAccount.accountUuid` in the same folder against the chat's account, because Zeron's existing account switcher can replace `~/.claude`.
- **Renewal.** Within 5 minutes of expiry, or after a 401, the module runs `claude auth status --json` in that folder. It is single-flight per folder, with a 10 s timeout and no model call. The output's email and org are checked against the chat's account, and the file is re-read.
- **Failures.** A mismatch means "can't host here". A persistent 401 means "sign in again".

**Chat-row fields.** These are synced registry fields:
- `providerSession` is a new top-level field, written once. It is also copied into the chat document's metadata:
  ```
  providerSession: { provider: "claude", sessionId, accountUuid, accountEmail, orgId }
  ```
- `hostUnavailable: { deviceId, reason }` is written by a host whose account check fails, cleared when the check passes again, and cleared by a new host.
- `config.harness = "claude-code-cloud"`. Any eligible engine restores it from `providerSession` if an older build wipes it. A `claude-code-cloud` config without a `providerSession` refuses to run.
- **Chat id** is a UUIDv5 of `claude:` plus the session id, so two importers converge on one row.
- **No other writes to `deviceId`.** For provider-run chats, every row write other than a host claim is a field-only update and never includes `deviceId`.

**Hosting.**
- **Eligibility.** A device is eligible when it advertises the device capability `provider-run-v1` (separate from the existing Cloud session capability) and has a login folder for the chat's account.
- **Takeover on use.** When the user opens or sends to the chat on an eligible device, that device takes over if the current host is offline (the relay's 75 s `host_offline` signal), lacks the capability, or is named in `hostUnavailable`. There is no background takeover.
- **The claim.** A new registry update op writes only `deviceId`, with a per-field clock set to the later of now + 5 s and one past the current clock. The claimant starts work only after the relay echoes the merged row with itself as host.
- **Demotion.** A watcher on chat rows stops the stream, the command drain and transcript writes as soon as `deviceId` changes away. `is_host` is re-checked before each outbound request.
- **Failing closed.** For chats with `providerSession`, a read error or a missing row means "not host".
- **Forks.** The host refuses fork and side-chat requests for these chats, including requests from older clients.

**Commands.**
- **Messages, interrupts and settings changes** use the Zeron command id as the event `uuid`. An uncertain delivery is always retried with its original uuid, after checking the history for it.
- **Prompt answers** use a UUIDv5 of the `request_id`, and are only sent while that request has no response in the mirrored history.
- **Create** is synchronous on the user's device and never queued or retried automatically. It carries a tag `zeron:<first prompt uuid>` (the chat id is a UUIDv5 of the session id, so it doesn't exist before the create succeeds; the first prompt's uuid is chosen first and is also the first event's uuid). If the response is lost, Zeron pages the list for the tag and adopts the session it finds, and failing that shows "Creation uncertain" with a link to claude.ai and a user-only "Create again", which makes a new create under a new first-prompt uuid.

**Transcript mirroring.**
- Each mirrored entry carries its provider `uuid` and `sequence_num`.
- `providerWatermark`, in the document metadata, is the highest fully committed sequence number, including events that map to no entry. Only the host advances it, after commit.
- A new host resumes with `from_sequence_num` set to the lower of the watermark and one less than the sequence number of the oldest unanswered `control_request`. It rebuilds the list of open prompts from the transcript before connecting.
- **Dedupe.** For each provider uuid, keep the entry with the lowest Loro element id and delete the others. This is the same on every device. The host runs the sweep after each batch, on takeover and on remote transcript changes. Clients also hide duplicates by provider uuid at render time.

**Discovery.**
- **Paging.** An eligible device reads each enabled account's list at most once a minute while the sidebar is visible, following `next_cursor` until sessions are older than 7 days.
- **Creating rows.** Rows for non-archived sessions are created with a low-clock upsert: the row clock and every field clock are the session's `created_at`, counter 0, plus the device suffix. It creates truly missing rows only, loses to every real write and to tombstones, and never moves a chat.
- **Display.** Title, status and unread come from the list as a UI overlay; they are not written to rows.
- **Archive and delete.**
  - A session archived on claude.ai makes the host delete the chat.
  - Archive in Zeron is Zeron-only.
  - Delete in Zeron archives the session.
  - A device that sees a tombstone for a session active again since the delete recreates the chat with a normal clock, and rebuilds its history.

**UI.**
- **Place chip.** "Anthropic cloud" appears under Zeron Cloud on eligible devices. Its Project menu offers GitHub projects, "New project from GitHub…" and "No project", and the model picker shows Claude models only.
- **Chat appearance.** A Claude-cloud mark. Panels are gated by `remote_workspace`.
- **Chat states.** "Queued, waiting for an eligible device", a failed state when a queued message expires, "Creation uncertain", "Sign in as <email> to continue", "Sign in again", "Archived on claude.ai", and the takeover note.
- **Settings.** A normal "Claude cloud sessions" setting, with per-account toggles (the device's login on by default) and "Add account". When the device's login changes, Zeron offers to add the previous account.

**Failure handling.**
- The whole feature turns off, saying why, on:
  - a `DISABLED` file in the feature's data folder;
  - a 401 that persists after one CLI renewal;
  - a 403 or 404 from list or create after the identity check;
  - an unrecognised event envelope.
- A 403 or 404 for one session affects only that chat.
- `Retry-After` is honoured, with exponential backoff on 429 and 5xx, and nothing retries in a loop.

**Shipping.** The work goes on a branch of the nightly repo, based on upstream `zeron/zgx`, and is listed in the v3 patch list with `enabled = false` until the live smoke test passes.

## Testing Decisions

**A good test** drives the system through the engine's public request surface, as the app does, and asserts only externally visible results:
- transcript contents;
- which device hosts the chat;
- what the fake provider server received;
- what states the chat shows.

It doesn't assert internal call sequences, private state or log lines.

**Seam 1, primary: two real engines, with fakes for the outside world.** Nearly everything is tested here.
- **Setup:** two engine instances with separate data folders and device ids, sharing a workspace and chat documents through the existing in-memory relay and registry bridge.
- **A fake Anthropic session server** replays the recorded spike sessions and enforces the observed semantics: uuid dedupe, first answer wins, replay strictly after N, non-idempotent create, paging, archived status, and 401/403/404 cases.
- **A stub `claude` executable** answers `auth status --json` and can simulate renewal, a mismatched account and logged-out states.
- **Required scenarios,** which are also the release gates:
  - create, stream and send;
  - steer, interrupt and prompt answers;
  - crash after send, with exactly one delivery;
  - two simultaneous claims;
  - a demoted host reconnecting;
  - takeover while a prompt is unanswered;
  - a lost create response;
  - an account switch on the host, which hands the chat over;
  - an older build wiping the config, which gets repaired;
  - duplicate entries converging;
  - discovery against a stale registry, without moving the chat;
  - archive, delete and revive;
  - a per-session 404 against a feature-wide auth failure.
- **Prior art:**
  - the engine integration tests that run two engines sharing a workspace doc through an in-memory bridge;
  - the device-routing tests with a fake device room relay;
  - the relay-delivery tests for processed-ledger dedupe;
  - the Claude harness tests that replay captured live frames and use fake CLI executables.

**Seam 2: a live smoke test against the real API.** It is ignored by default, like the existing live tests. It creates a session with no repo, sends, streams, answers an AskUserQuestion and archives. It runs before the patch is enabled, and is the only test that catches Anthropic changing the API.

**UI.** No automated seam; the UI reads data that Seam 1 covers. The place chip, the mark, panel gating and the chat states are checked by hand in the running app on PC and Laptop.

## Out of Scope

- **Zeron's MCP tools inside the cloud session** (phase 2). It needs a tunnel from a device, because the relay can't be changed, and would be off by default.
- Local repos without a GitHub remote (bundle upload).
- Linux SG as an always-on preferred host. It needs a Linux build of the branch.
- Hosting with no device at all. It needs a self-hosted relay.
- Providers other than Anthropic, though names stay provider-neutral.
- Creating or editing cloud environments. Zeron uses the account's existing one and points the user to claude.ai/code if there is none.
- Hosting provider-run chats on Zeron Cloud machines.
- Starting provider-run chats from the Zeron phone app. The relay's model lists don't include this harness.
- Background or automatic takeover between devices, and any strict single-host lock.
- Continuing a session from a different Claude account. Anthropic doesn't allow it.

## Further Notes

- **Risk accepted by the user.** This uses Anthropic's private session API with a subscription login, which their terms don't allow, and on a Team plan enforcement could affect the whole Team. The design keeps exposure low:
  - honest identity;
  - logins held by the CLI;
  - only the session endpoints;
  - human-paced traffic;
  - a scoped kill switch.
- **Evidence.** The protocol spike holds scripts and redacted fixtures for every claim here. Its fixtures seed the fake provider server.
- **Leftovers from the spike.** A cloud environment named "Zeron spike" remains on the account; remove it in claude.ai/code. The GitHub-repo session path wasn't exercised, because the Claude GitHub App lacks access to the test repo; the live smoke test should cover it once access is granted.
- **Review.** A three-reviewer council scored the design at least 9/10 on every part after four rounds. Its reviewers noted this is confidence in the design, not the implementation; the release-gate scenarios above are what certify the build.
