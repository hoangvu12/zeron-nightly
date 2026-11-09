# Handoff: provider-run chats (spec hoangvu12/zeron-nightly#1)

Stopped on the PC on 2026-11-10 at the user's request, to continue on the Laptop.
This note lives on its own branch `handoff/provider-run-chats` (on the `nightly` remote) so it never ships with the feature. Delete the branch when you're done with it.

## Where things are

| Ticket | State | Branch / commit on `nightly` |
|---|---|---|
| #2 registry clocks | **Closed** | merged in `feat/provider-cloud-sessions` |
| #3 tracer | **Closed** | merged |
| #7 login renewal | **Closed** | merged (`3c3843537`) |
| #5 steer / stop / prompts | **Merged, not yet closed.** Review fixes `e2952f51e` merged as `76c26c49a`; the verification test run was stopped before it finished | `feat/provider-cloud-sessions` at `76c26c49a` |
| #4 app UI | **In progress, all committed.** It had merged integration up to `b5deae3ad`, added the sign-in state rendering, and was doing the manual check in the running app | `ticket/4-app-ui` at `be0ccfb1f` |
| #8 replay / watermark / dedupe | **In progress, WIP commit.** It hadn't compiled yet and hadn't merged `76c26c49a` | `ticket/8-replay` at `9f0b7416c` |
| #6, #9–#16 | Not started | — |

**Integration branch:** `feat/provider-cloud-sessions` at `76c26c49a`. Always push it to `nightly` only, never to `origin` (zeronsh/zeron).

## Next steps, in order

1. **Verify #5 and close it.** Check out `feat/provider-cloud-sessions` and run:
   ```
   cargo test -p zeron-harness --test claude_cloud --test claude_cloud_login   # expect 18 and 12
   cargo test -p zeron-engine --test provider_run_chats --test provider_run_sign_in --test steer_now   # expect 11, 3, 4
   cargo test -p zeron-harness --lib claude    # 92
   cargo test -p zeron-proto provider_cloud
   ```
   If they pass, close #5 with a comment that summarises the ticket and its review fixes (see the summary at the end of this note).
2. **#8 (critical path: it unblocks #9 and #12).** Continue on `ticket/8-replay`:
   - Merge `feat/provider-cloud-sessions` (`76c26c49a`) in. Expect conflicts in `crates/harness/src/claude_cloud/session.rs` and `mirror.rs`, which #5's fixes rewrote heavily. Keep all of #5's behaviour and layer the resume, watermark and dedupe on top.
   - Finish the WIP: entry metadata (provider uuid and `sequence_num`), the `providerWatermark` in doc metadata, resume from min(watermark, oldest open `control_request` − 1), rebuilding open prompts before connecting, the lowest-Loro-id dedupe sweep plus render-time hide, and own-echo matching that survives restarts (transcript uuids, not the in-memory `was_delivered`).
   - Expose a "resume as new host" entry point for #12.
   - Engine tests should do a real restart and use the two-engine bridge (`crates/engine/tests/workspace_sync.rs`).
3. **#4.**
   - Merge `76c26c49a` into `ticket/4-app-ui`.
   - Finish the manual check: the place chip shows "Anthropic cloud" only with the "Claude cloud sessions" setting on, the model picker shows Claude models only, the chat has a Claude-cloud mark with files/diff/terminal/background/fork/side chats hidden via `remote_workspace`, and the `providerState` label shows.
   - There's a fake-server example on that branch for running the app without the real API.
   - Then review, merge and close.
4. **Then follow the frontier.** A ticket is unblocked when all its "blocked by" tickets are closed:
   - #6 after #4.
   - #9 after #2, #4 and #8.
   - #12 after #2, #5 and #8.
   - #10 and #11 after #9.
   - #13 after #12.
   - #14 after #7, #9 and #12.
   - #15 after #9 and #12.
   - #16 last.

## Decisions and notes already recorded

- **Create tag** (user decision, see #11 comment): it is `zeron:<first prompt uuid>`, not `zeron:<chatId>`. When #11 lands, amend design §4 and the spec's Commands › Create to match.
- **"Sign in as"** is owned by #12 (see #12 comment). #7 exposes `ProviderCloudStatus::cant_host(account_uuid)` and `AccountLogin::{Works, SignInAgain, CantHost}` through `ProviderCloud::status()`. #12 publishes `hostUnavailable` and sets `SignInAs` only when no eligible device can host.
- **Handed on:**
  - To #13: a `duplicate: true` ack starts no turn.
  - To #15: an unrecognised event envelope must turn the feature off (today it is dropped), plus a persistent kill switch, the DISABLED file and 403/404 triggers.
- **Not verified against the real API**, so the #16 smoke test should cover them:
  - the `set_model` request and reply shape (invented);
  - steering mid-turn (run-after vs fold-in is inferred);
  - `interrupt` with `cancel_queued` (inferred from the `interrupt_cancel_queued_v1` capability);
  - whether the server keeps custom tags.
- **Known trade-off from #5:** while a send waits to retry (up to about 10 s with a long `Retry-After`), a Stop is handled after the wait.
- **Windows:** `cargo test -p zeron-engine --lib` compiles thanks to `48e617222`. These test files are unix-only and still don't compile on Windows (unrelated): `crates/preview/src/login.rs`, `crates/engine/tests/adapter_chaos_live.rs` and the harness `acp_registry_live`.
- **Process used on the PC:**
  - one implementer per ticket in its own worktree, branched from the integration head;
  - tests first, at the engine's public seam;
  - after merging, a two-axis `code-review` (Standards and Spec, as parallel sub-agents), with fixes sent back to whoever owns that code before closing;
  - close each ticket with a summary comment.
  - Starting a ticket early from an unmerged blocker's branch saved a lot of time.

## Hard rules (from the user's brief)

- `gh` always with `-R hoangvu12/zeron-nightly`. Never create, comment on or close anything on zeronsh/zeron, and never push to `origin`. Don't close or edit #1.
- **Real Anthropic API** (only #11's tag check and #16's smoke test):
  - never print, log or commit a token, refresh token, org or account id;
  - never refresh OAuth yourself (`claude auth status --json` does it);
  - `User-Agent: zeron/<version>`, with no client-platform header;
  - at most 3 real sessions per ticket, human-paced, and archive every session you create;
  - never call environment endpoints.
- **#16** also needs:
  - the release-gate scenarios run on the Laptop;
  - a **disabled** entry for the branch in zeron-nightly's `patches-v3.toml` (repo hoangvu12/zeron-nightly, branch `main`). **Ask the user before pushing that change.**
- **Protocol evidence** (spike scripts and fixtures) lives on the PC at `C:\Users\ADMIN\Desktop\nguyenvu\claude-cloud-spike`. It isn't on the Laptop. The fixtures needed for tests are already copied, redacted, into `crates/provider-fake/fixtures/`.
- Report progress to the user after each wave: which tickets closed, which are in flight, any blockers.

## #5 summary for its closing comment

- Steers post immediately under the message id.
- Stop sends `interrupt` + `cancel_queued` only for a real Stop command, under its command id. Shutdown, sign-out and space delete never interrupt the session.
- Prompt answers go under a UUIDv5 of `request_id`, only after a history check past the prompt's sequence number shows no `control_response`.
- Model and permission/plan controls are sent only for changes made in Zeron, relative to the session state learned from `system/init`.
- Bounded retries under the original uuid, honouring `Retry-After`.
- Bounded reconnects with backoff.
- Messages typed on claude.ai are mirrored as the person's messages.
- Two-axis review done; fixes in `e2952f51e`.
