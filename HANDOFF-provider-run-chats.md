# Handoff: provider-run chats (spec hoangvu12/zeron-nightly#1)

Paused on the Laptop on 2026-11-10 at the user's request. Everything is pushed to the `nightly` remote (hoangvu12/zeron-nightly); nothing went to `origin`.
This note lives on its own branch `handoff/provider-run-chats` so it never ships with the feature. Delete the branch when you're done with it.

## Where things are

**Integration branch:** `feat/provider-cloud-sessions` at `9115e8a3b`. Every ticket below marked "Closed" is merged there, and its suites pass on that head.

| Ticket | State | Branch on `nightly` |
|---|---|---|
| #2, #3, #4, #5, #6, #7, #8, #9, #12 | **Closed**: merged, reviewed, review fixes merged | integration |
| #10 + #11 | **Built and reviewed; review fixes partly done.** The engine half of the fixes is committed (`23a93b7a8`). The rest is a WIP commit (`8dbfd9cfa`) that has **not been built or tested** | `ticket/10-11` |
| #13 + #15 | **In progress.** WIP commit, not built or tested | `ticket/13-15` at `957e8fb3c` |
| #14 | **In progress.** The engine half is committed (`0cfd989e2`); the settings and sidebar UI are a WIP commit, not built or tested | `ticket/14-accounts` at `08ee25c06` |
| #16 | Not started | — |

**Every WIP commit says so in its message.** Before going further on a WIP branch, build it, then run its tests (`-j 4`; see the Windows notes).

## Next steps, in order

1. **#10 + #11** (`ticket/10-11`). Build and test the WIP, then finish the review fixes listed below.
   - First merge the integration branch: `ticket/10-11` is behind `9115e8a3b`, which has #6 with its review fixes.
   - Then merge into integration, run a two-axis review of the fix commits, and close #10 and #11.

   Review must-fixes:
   - **#11, create deadline:** one overall deadline on the create, shorter than the composer's 150 s wait, plus per-request timeouts on list reads. Clear "Creation uncertain" when a chat with the matching first-prompt uuid appears. The engine side is in `23a93b7a8`; check the composer side.
   - **#11, dropped connection:** `create_outcome` must map `RpcError::Closed`/`Transport`/a failed join to Uncertain for provider-run creates.
   - **#10, archives after delete:** a delete must never silently fail to archive. The archives a delete owes are now stored and retried (`23a93b7a8`); verify that, and verify the out-of-window path that reads a single session.
   - **#11, buttons:** the "Creation uncertain" buttons must copy the transcript's "Log in" button, tinted warning. Take light and dark screenshots.

   Also fix:
   - **#10, unarchive while the host is offline:** this must not delete the chat (in `23a93b7a8`; verify).
   - **#10, revive margin:** in `23a93b7a8`.
   - **Test sleep:** replace the `sleep(300ms)` in `provider_run_archive.rs`.
   - **`chatId`:** keep sending it until this ships.
   - **Cleanups:** one shared page iterator, an `hlc_ms` helper, stale doc comments.

   From the live check (below):
   - remove `TAGS_KEPT` and its dead branches;
   - update design §4 to say tags were confirmed on 2026-11-10;
   - make the single-session read lenient, with a fallback to paging the list;
   - add a bounded retry on `NotSent` for every call.

   Amendment text for #1, Commands › Create. #1 must not be edited, so put this in #11's closing comment: "Create is synchronous on the user's device and never queued or retried automatically. It carries a tag `zeron:<first prompt uuid>`. If the response is lost, Zeron pages the list for the tag and adopts the session it finds, and failing that shows 'Creation uncertain' with a link to claude.ai and a user-only 'Create again', which makes a new create under a new first-prompt uuid."
2. **#13 + #15** (`ticket/13-15`). Build and test the WIP, then keep going. Owed items:
   - **Flaky missing reply after a takeover**, about 2 runs in 10. The reproducer is `two_messages_drained_together_after_a_takeover_are_answered` in `provider_run_hosting.rs` (ignored); the suspected cause is in #8's mirror or #5's steering. Find the root cause, fix it, un-ignore the test and run it about 30 times.
   - **Duplicate ack:** a `duplicate: true` ack starts no turn.
   - **"Queued, waiting for an eligible device":** an offline host means the chat waits. "Sign in as" is only for when no device can host.
   - **#15 failure handling:** an unrecognised event envelope turns the feature off; a persistent kill switch; the DISABLED file; 403/404 triggers; config repair from `providerSession`.
   - **No cloud environment:** an account that has never used claude.ai/code fails with "no cloud environment". Make it a typed error, and show "Start one session on claude.ai/code first" with a link. Never retry it and never treat it as uncertain.
   - **Fork and side-chat refusal** already exists (#12); just test it.
3. **#14** (`ticket/14-accounts`). Build and test the WIP UI, then finish it. Its first commit also took over #9's follow-up: the importer claims a chat's host only at the low clock and starts work after the echo, using #12's mechanism.
4. **#16** last:
   - the release-gate scenarios on the Laptop;
   - the real-API smoke test (see "Still not verified");
   - a **disabled** entry in zeron-nightly's `patches-v3.toml` (repo hoangvu12/zeron-nightly, branch `main`). **Ask the user before pushing that.**

## Live API results (2026-11-10, user-approved, 1 real session, archived)

- **Tags are kept:** a create's `zeron:<uuid>` tag is found in the session list. #11's design is confirmed.
- **The single-session read doesn't parse.** `GET /v1/code/sessions/{id}` answers, but its body doesn't decode into `SessionInfo` ("an answer Zeron can't read"). #10 relies on that read. To learn its real shape, ask the user before making one read-only call that prints only the key names.
- **Create needs an existing environment.** It fails on an account with no claude.ai/code session ("no cloud environment"). The user has since started one.
- **The Laptop's network is unstable.** It resets about 60% of TLS handshakes to `api.anthropic.com`, and no proxy is configured. The check only got through with retries on `NotSent`.
- **Re-running the check:** `tagcheck/live-retry` (`7f5fe1161`) has the live test with those retries. Run it with `ZERON_LIVE_TAG_CHECK=1 cargo test -p zeron-harness --test claude_cloud_tag_live -- --ignored --nocapture`, outside the tool sandbox, which resets these connections.

## Still not verified against the real API (for #16)

- **Session controls:** the `set_model` request shape, steering mid-turn, and `interrupt` with `cancel_queued`.
- **List status words:** `running`/`working`/`busy` and `requires_action`/`waiting`.
- **GitHub source:** `{"type":"git_repository","url","revision"}` with empty `outcomes` (#6). Make the GitHub-repo session mandatory in #16.
- **Single-session read:** the response shape (above).

## Decisions and notes already recorded

- **Create tag:** `zeron:<first prompt uuid>` (user decision). Design §4 and the spec doc in the repo are amended on `ticket/10-11`.
- **"Sign in as"** belongs to #12 and is done. It shows only when the host lacks the capability or has reported `hostUnavailable`, never just because the host is offline.
- **Single checks:**
  - `WorkspaceHost::is_provider_run(&Chat)` / `is_provider_run_id` is the one check for "is this a provider-run chat".
  - `registry_synced()` is the one "registry trusted" check; `registry_current()` calls it.
- **Defaults and gates:**
  - "Claude cloud sessions" is off by default and applied at engine start. Engine tests that use the fake harness must turn it on before the engine starts.
  - `provider-projects-v1` gates "New project from GitHub…".
- **Follow-ups** are recorded in the closing comments of #4, #6, #8, #9 and #12. The bigger ones:
  - after an offline gap, missed turns land after a new message;
  - idle chats don't catch up on boot;
  - discovered sessions aren't grouped under projects (story 8);
  - imported chats keep a stream open for 30 minutes.
- **Known flakes under machine load:** `a_stale_registry_cannot_move_a_chat_or_overwrite_its_fields` (#9) and `message_queue`.

## Windows and machine notes

- **Memory:** 32 GB isn't enough for six parallel cargo builds; use `-j 4`. A "memory allocation failed" or `STATUS_STACK_BUFFER_OVERRUN` compile error means memory, not code.
- **Unix-only test files** don't compile on Windows: `crates/preview/src/login.rs`, `crates/engine/tests/adapter_chaos_live.rs`, the harness's `acp_registry_live`, and anything using `fake-claude.sh`.
- **Known failures:** 4 `device_routing` tests already fail on integration. Target specific tests rather than whole crates.
- **App and fake server:** the app is `crates/ui` (it builds `apps/zeron`). The fake server is `crates/provider-fake/examples/serve.rs`.
- **Main checkout:** `C:\Users\HP MEDIA\Desktop\nguyenvu\zeron` has the user's uncommitted zgx work. Never write there; use worktrees.

## Process used

- One implementer per ticket, each in its own worktree, branched from the integration head or early from a blocker's branch.
- Tests first, at the engine's public seam.
- After merging, a two-axis `code-review` (Standards and Spec), run alongside the next ticket.
- Fixes go back to the implementer. A ticket is closed with a summary comment once its fixes are merged.

## Hard rules (from the user's brief)

- **GitHub:**
  - `gh` always with `-R hoangvu12/zeron-nightly`.
  - Never create, comment on or close anything on zeronsh/zeron, and never push to `origin`.
  - Don't close or edit #1.
- **Real Anthropic API:**
  - never print, log or commit a token, refresh token, org or account id;
  - never refresh OAuth yourself;
  - send `User-Agent: zeron/<version>`, with no client-platform header;
  - at most 3 real sessions per ticket, human-paced, and archive every session you create;
  - never call environment endpoints.
- **#16:** ask the user before pushing the `patches-v3.toml` change.
