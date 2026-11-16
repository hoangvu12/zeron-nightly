-- Chats on sandboxes (spec #37), as the host reads them from the engine that
-- keeps their provider (src/sandboxes.rs). What's said of one is
-- screens/chat/sandbox_machine's.
--
--   sandboxes.steps(chat)      its sandbox (tracked), nil for a chat not on a
--                              sandbox this device keeps: { state = "starting"
--                              | "ready" | "failed" | "gone", provider,
--                              provider_name?, step?, error?, disk_full }
--   sandboxes.follow(chat)     follow it (opening a chat): asks the provider
--                              whether it's still there, never waking it
--   sandboxes.fresh(chat)      Start fresh: a new sandbox, the chat's pushed
--                              work with it
--   sandboxes.fresh_state(chat)  { busy?, failure? } of the last Start fresh
--                              (tracked)
--
-- Each host call is a field (`ask_*`) a fixture can answer in the host's place.

local M = {}

local STEPS = ui.shared("sandboxes", {} :: { [string]: any })
M.fresh_states = ui.state("account_sandboxes_fresh", {} :: { [string]: { busy: boolean?, failure: string? } })

function M.ask_follow(chat: string)
	ui.invoke("sandboxes.follow", chat)
end
function M.ask_fresh(chat: string): any
	return ui.call("sandboxes.fresh", chat)
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

function M.fresh(chat: string)
	local now = M.fresh_states:peek()[chat]
	if now and now.busy then
		return
	end
	M.fresh_states:set_key(chat, { busy = true } :: any)
	M.ask_fresh(chat):after(function()
		M.fresh_states:set_key(chat, nil :: any)
	end, function(e: any)
		local word = if type(e) == "table" and type(e.message) == "string" then e.message else "failed"
		M.fresh_states:set_key(chat, { failure = word } :: any)
	end)
end

return M
