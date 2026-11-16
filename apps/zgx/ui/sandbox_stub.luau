-- Chats on sandboxes, answered here as the host would (no engine in a test
-- host): account/sandboxes' calls. Required by ui/chat_fixture.luau;
-- tests/sandbox_machine.test.luau drives it.
--
-- No chat is on a sandbox until a command says so. What was asked, in order,
-- is `sandbox_stub_calls` (a state). `ui.command`s change what the engine
-- says of the open chat's sandbox, as it would on its own:
--   sandbox_stub.ready         its sandbox is up
--   sandbox_stub.gone          deleted outside Zeron (on Docker Sandbox)
--   sandbox_stub.gone_unnamed  ... on a provider whose name isn't known
--   sandbox_stub.disk_full     its disk is full
--   sandbox_stub.fail_next     the next Start fresh fails ("offline")
--   sandbox_stub.hold_next     the next Start fresh stays on its way
--   sandbox_stub.started       a fresh sandbox is starting

local sandboxes = require("@account/sandboxes") :: any
local store = require("@app/screens/chat/store")

local calls = ui.state("sandbox_stub_calls", {} :: { string })
local steps = ui.state("sandbox_stub_steps", {} :: { [string]: any })
local fail = ui.state("sandbox_stub_fail", false)
local hold = ui.state("sandbox_stub_hold", false)
-- (Kept across launches in the app: here, each fixture starts with none.)
sandboxes.fresh_states:set({})

local function log(entry: string)
	calls:push(entry)
end

sandboxes.read = function(): { [string]: any }
	return steps:get()
end
sandboxes.ask_follow = function(chat: string)
	log(`follow:{chat}`)
end
sandboxes.ask_fresh = function(chat: string): any
	log(`fresh:{chat}`)
	local p = {}
	function p.after(_self: any, ok: ((any) -> ())?, bad: ((any) -> ())?)
		if hold:peek() then
			hold:set(false)
		elseif fail:peek() then
			fail:set(false)
			if bad then
				bad({ message = "offline" })
			end
		elseif ok then
			ok(true)
		end
	end
	return p
end

local function open_chat(): string?
	local s = store.session:peek()
	return s and s.id
end

local function say(fields: { [string]: any })
	local chat = open_chat()
	if not chat then
		return
	end
	local s: { [string]: any } = { state = "ready", provider = "docker", provider_name = "Docker Sandbox", disk_full = false }
	-- (`false` for a field it doesn't have: a nil in a table isn't there.)
	for k, v in fields do
		s[k] = if v == false and k ~= "disk_full" then nil else v
	end
	local now = table.clone(steps:peek())
	now[chat] = s
	steps:set(now)
end

local CASES: { [string]: { [string]: any } } = {
	ready = {},
	gone = { state = "gone", error = "The sandbox was deleted outside Zeron." },
	gone_unnamed = { state = "gone", provider = "elsewhere", provider_name = false },
	disk_full = {
		state = "failed",
		step = "getCode",
		error = "The sandbox's disk is full. Free some space, or start fresh.\nwrite error: No space left on device",
		disk_full = true,
	},
	started = { state = "starting", step = "create" },
}
for case, fields in CASES do
	ui.command {
		id = `sandbox_stub.{case}`,
		title = `The open chat's sandbox: {case}`,
		run = function()
			say(fields)
		end,
	}
end
ui.command {
	id = "sandbox_stub.fail_next",
	title = "The next Start fresh fails",
	run = function()
		fail:set(true)
	end,
}
ui.command {
	id = "sandbox_stub.hold_next",
	title = "The next Start fresh stays on its way",
	run = function()
		hold:set(true)
	end,
}

return {}
