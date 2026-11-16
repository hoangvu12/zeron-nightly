-- Chats on sandboxes (spec #37), as the host reads them from the engine that
-- keeps their provider (src/sandboxes.rs). What's said of one is
-- screens/chat/sandbox_machine's.
--
--   sandboxes.steps(chat)      its sandbox (tracked), nil for a chat not on a
--                              sandbox this device keeps: { state = "starting"
--                              | "ready" | "failed" | "gone" | "sleeping"
--                              | "asleep" | "waking", provider,
--                              provider_name?, step?, error?, disk_full }
--   sandboxes.follow(chat)     follow it (opening a chat): asks the provider
--                              whether it's still there, never waking it
--   sandboxes.fresh(chat)      Start fresh: a new sandbox, the chat's pushed
--                              work with it
--   sandboxes.fresh_state(chat)  { busy?, failure? } of the last Start fresh
--                              (tracked)
--   sandboxes.wake(chat)       Wake a sandbox asleep; wake_state(chat) as
--                              fresh_state
--
-- Each host call is a field (`ask_*`) a fixture can answer in the host's place.

local M = {}

local STEPS = ui.shared("sandboxes", {} :: { [string]: any })
M.fresh_states = ui.state("account_sandboxes_fresh", {} :: { [string]: { busy: boolean?, failure: string? } })
M.wake_states = ui.state("account_sandboxes_wake", {} :: { [string]: { busy: boolean?, failure: string? } })

function M.ask_follow(chat: string)
	ui.invoke("sandboxes.follow", chat)
end
function M.ask_fresh(chat: string): any
	return ui.call("sandboxes.fresh", chat)
end
function M.ask_wake(chat: string): any
	return ui.call("sandboxes.wake", chat)
end
-- (Read through here, so a fixture can stand in for the host's.)
function M.read(): { [string]: any }
	return STEPS:get() or {}
end

function M.steps(chat: string?): any
	if not chat then
		return nil
	end
	return M.read()[chat]
end

function M.follow(chat: string?)
	if chat then
		M.ask_follow(chat)
	end
end

function M.fresh_state(chat: string?): { busy: boolean?, failure: string? }
	return (chat and M.fresh_states:get()[chat]) or {}
end

-- An action asked of the host for a chat, its { busy?, failure? } kept in
-- `states` meanwhile.
local function ask(states: any, chat: string, call: (string) -> any)
	local now = states:peek()[chat]
	if now and now.busy then
		return
	end
	states:set_key(chat, { busy = true } :: any)
	call(chat):after(function()
		states:set_key(chat, nil :: any)
	end, function(e: any)
		local word = if type(e) == "table" and type(e.message) == "string" then e.message else "failed"
		states:set_key(chat, { failure = word } :: any)
	end)
end

function M.fresh(chat: string)
	ask(M.fresh_states, chat, M.ask_fresh)
end

function M.wake_state(chat: string?): { busy: boolean?, failure: string? }
	return (chat and M.wake_states:get()[chat]) or {}
end

-- Wake: its sandbox started again, its engine with it. (A send wakes it
-- too, on the engine's side.)
function M.wake(chat: string)
	ask(M.wake_states, chat, M.ask_wake)
end

return M
