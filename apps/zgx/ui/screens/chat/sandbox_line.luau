-- What a chat on a place says of its sandbox's steps (spec #37): one row
-- above the composer, its own and always shown (ds/composer_band), never
-- folded into the deck's alerts.
--
--   starting      the working ring, the step it's on ("Installing Zeron"),
--                 how long (muted, "12s") and the 8 steps as segments
--                 (screens 05, 15)
--   waking        the ring, "Waking sandbox", how long (09, 11)
--   a script      the ring, "Running setup.sh" (13)
--   step failed   "Getting the code failed", its error, Run again
--   script failed "Setup failed", its path in mono, Run again and × (12)
--
-- Ready, asleep (screens/chat/sandbox_machine says that), or not on a place:
-- nothing. Pure: `steps` as account/sandboxes has them (`{ state, step?,
-- since, error?, progress?, scripts? }`, `since` and `now` in Unix seconds),
-- so tests read it without `ui`.
--
--   line(steps, now, dismissed?)  the band, or nil (`dismissed`: failed
--                                 scripts' keys, put away with their ×)
--   script_key(run)               a failed script's key
--   ready(steps)                  the transcript's "Sandbox ready 52s":
--                                 { fresh, took, at } once its start got ready
--   with_ready(messages, ready, text)  the transcript's messages with that
--                                 row among them, where it happened

local M = {}

-- ds/composer_band's Band, as data.
export type Band = {
	kind: string,
	title: string,
	-- Its glyph (nil with `spinner`), in its tone's ink.
	icon: string?,
	spinner: boolean?,
	tone: string?,
	-- Muted after the title (`mono`: a path).
	detail: string?,
	mono: boolean?,
	-- How long, muted after it.
	time: string?,
	-- Each starting step's state ("done", "running", …): its segments.
	progress: { string }?,
	-- Its action's label, and whether its × puts it away.
	action: string?,
	dismissable: boolean?,
	-- (A failed script's, for its ×.)
	key: string?,
}

local function say(key: string, english: string, args: { [string]: any }?): string
	if (ui :: any) == nil then
		return english
	end
	return ui.t(`chat.sandbox.{key}`, args):get()
end

-- Each step, as the band names it while it runs, and as what failed.
local STEPS: { [string]: { key: string, english: string, noun: string } } = {
	create = { key = "create", english = "Creating sandbox", noun = "Create" },
	start = { key = "start", english = "Starting sandbox", noun = "Start" },
	installZeron = { key = "install", english = "Installing Zeron", noun = "Install" },
	signIn = { key = "sign_in", english = "Signing in", noun = "Sign-in" },
	secrets = { key = "secrets", english = "Adding secrets", noun = "Secrets" },
	getCode = { key = "get_code", english = "Getting the code", noun = "Getting the code" },
	setup = { key = "setup", english = "Running setup", noun = "Setup" },
	ready = { key = "ready", english = "Starting Zeron", noun = "Ready" },
	wake = { key = "wake", english = "Waking sandbox", noun = "Wake" },
	sleep = { key = "sleep", english = "Going to sleep", noun = "Sleep" },
	delete = { key = "delete", english = "Deleting sandbox", noun = "Delete" },
}

function M.step_name(step: string?): string
	local s = step and STEPS[step]
	if not s then
		return say("step.other", "Setting up sandbox")
	end
	return say(`step.{s.key}`, s.english)
end

-- What a step's script is called when it fails ("Setup failed").
function M.script_failed(step: string?): string
	local s = step and STEPS[step]
	if not s then
		return say("script_failed.other", "Script failed")
	end
	return say(`script_failed.{s.key}`, `{s.noun} failed`)
end

-- How long, compactly: "8s", "1m 05s".
function M.elapsed(seconds: number): string
	local s = math.max(0, math.floor(seconds))
	if s < 60 then
		return `{s}s`
	end
	return string.format("%dm %02ds", s // 60, s % 60)
end

function M.script_key(run: { script: string?, name: string?, since: number? }): string
	return `{run.script or run.name or ""}@{run.since or 0}`
end

local function script(steps: any, state: string, dismissed: { [string]: boolean }?): any?
	for _, run in steps.scripts or {} do
		if run.state == state and not (dismissed and dismissed[M.script_key(run)]) then
			return run
		end
	end
	return nil
end

-- The band, or nil for none.
function M.line(steps: any?, now: number, dismissed: { [string]: boolean }?): Band?
	if not steps then
		return nil
	end
	if steps.state == "failed" and steps.step ~= "wake" and not steps.disk_full then
		-- A step that failed holds the chat up: no ×, Run again.
		local name = M.step_name(steps.step)
		return {
			kind = "step_failed",
			tone = "danger",
			icon = "danger-triangle",
			title = say("failed", `{name} failed`, { step = name }),
			detail = if steps.error and steps.error ~= "" then steps.error else nil,
			action = say("run_again", "Run again"),
		}
	end
	if steps.state == "failed" or steps.state == "gone" then
		-- (A wake that failed, a full disk, deleted: sandbox_machine's.)
		return nil
	end
	local running = script(steps, "running")
	if running and (steps.state == "starting" or steps.state == "ready" or steps.state == "waking") then
		return {
			kind = "script",
			spinner = true,
			title = say("running_script", `Running {running.name}`, { name = running.name }),
		}
	end
	if steps.state == "starting" then
		return {
			kind = "starting",
			spinner = true,
			title = M.step_name(steps.step),
			time = M.elapsed(now - (steps.since or now)),
			progress = steps.progress,
		}
	elseif steps.state == "waking" then
		return {
			kind = "waking",
			spinner = true,
			title = say("waking", "Waking sandbox"),
			time = M.elapsed(now - (steps.since or now)),
		}
	end
	local failed = script(steps, "failed", dismissed)
	if failed then
		return {
			kind = "script_failed",
			tone = "danger",
			icon = "danger-triangle",
			title = M.script_failed(failed.step),
			detail = if failed.script and failed.script ~= "" then failed.script else failed.name,
			mono = true,
			action = say("run_again", "Run again"),
			dismissable = true,
			key = M.script_key(failed),
		}
	end
	return nil
end

-- The transcript's row once its last start got ready: how long it took,
-- whether it was Start fresh, and when (to place it). Nil while it starts,
-- or with no start known.
function M.ready(steps: any?): { fresh: boolean, took: string, at: number }?
	local started = steps and steps.started
	if type(started) ~= "table" or type(started.ready_at) ~= "number" or type(started.at) ~= "number" then
		return nil
	end
	return {
		fresh = started.fresh == true,
		took = M.elapsed(started.ready_at - started.at),
		at = started.ready_at,
	}
end

-- The ready row's id in the transcript.
M.READY_ID = "\0sandbox-ready"

-- The transcript's messages (ds/transcript's, oldest first) with the ready
-- row among them: before the first reply that began once it was ready (so
-- after the prompt that waited for it, screens 06–11), else before the
-- first later prompt (never the chat's first: that one waited for it), else
-- at the end (Start fresh, nothing sent since: screen 16). Without `ready`,
-- the messages as they are.
function M.with_ready(messages: { any }, ready: { took: string, at: number }?, text: string): { any }
	if not ready then
		return messages
	end
	local at = #messages + 1
	local first_prompt: number? = nil
	for i, m in messages do
		if m.role == "user" and not first_prompt then
			first_prompt = i
		end
		if m.role == "assistant" and type(m.started) == "number" and m.started >= ready.at then
			at = i
			break
		end
		if m.role == "user" and i ~= first_prompt and type(m.sent) == "number" and m.sent >= ready.at then
			at = i
			break
		end
	end
	local out = table.clone(messages)
	table.insert(out, at, {
		id = M.READY_ID,
		role = "event",
		icon = "check-circle",
		tone = "success",
		text = text,
		detail = ready.took,
	})
	return out
end

return M
