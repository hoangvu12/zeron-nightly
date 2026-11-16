-- What a chat on a place says while its sandbox starts (spec #37, screen
-- 05): one compact line above the composer, the step it's on and how long
-- it's been at it; a step that failed says so, with its error and Run again
-- (screen 12). Ready, or not on a place: nothing. Pure: `steps` as
-- account/sandboxes has them (`{ state, step?, since, error? }`, `since`
-- and `now` in Unix seconds), so tests read it without `ui`.

local M = {}

export type Notice = { text: string, tone: string?, spinner: boolean?, action: string? }

local function say(key: string, english: string, args: { [string]: any }?): string
	if (ui :: any) == nil then
		return english
	end
	return ui.t(`chat.sandbox.{key}`, args):get()
end

-- Each step, as the line names it.
local STEPS: { [string]: { key: string, english: string } } = {
	create = { key = "step.create", english = "Creating the sandbox" },
	start = { key = "step.start", english = "Starting the sandbox" },
	installZeron = { key = "step.install", english = "Installing Zeron" },
	signIn = { key = "step.sign_in", english = "Signing in" },
	secrets = { key = "step.secrets", english = "Adding secrets" },
	getCode = { key = "step.get_code", english = "Getting the code" },
	setup = { key = "step.setup", english = "Running setup" },
	ready = { key = "step.ready", english = "Starting Zeron" },
}

function M.step_name(step: string?): string
	local s = step and STEPS[step]
	if not s then
		return say("step.other", "Setting up the sandbox")
	end
	return say(s.key, s.english)
end

-- How long, compactly: "8s", "1m 05s".
function M.elapsed(seconds: number): string
	local s = math.max(0, math.floor(seconds))
	if s < 60 then
		return `{s}s`
	end
	return string.format("%dm %02ds", s // 60, s % 60)
end

-- The line, or nil for none.
function M.line(steps: { state: string, step: string?, since: number?, error: string? }?, now: number): Notice?
	if not steps then
		return nil
	end
	if steps.state == "starting" then
		local name = M.step_name(steps.step)
		local took = M.elapsed(now - (steps.since or now))
		return { text = say("starting", `{name} · {took}`, { step = name, elapsed = took }), spinner = true }
	elseif steps.state == "failed" then
		local name = M.step_name(steps.step)
		local text = if steps.error and steps.error ~= ""
			then say("failed_because", `{name} failed: {steps.error}`, { step = name, error = steps.error })
			else say("failed", `{name} failed`, { step = name })
		return { tone = "danger", text = text, action = say("run_again", "Run again") }
	end
	return nil
end

return M
