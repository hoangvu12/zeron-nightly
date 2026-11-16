-- Save as template (spec #37): a chat's sandbox saved through its provider,
-- by the engine that keeps it (src/sandbox_templates.rs). Named apart from
-- the interface's own templates (`templates.*`). What a save says is
-- screens/shell/save_template's.
--
--   sandbox_templates.check(chats)    ask which chats run on a sandbox kept
--                                     here (each once)
--   sandbox_templates.can_save(chat)  whether it does (tracked)
--   sandbox_templates.save(chat, name)  saves it; while it does, and once
--                                     it's done, its `notice(chat)` says so
--   sandbox_templates.notice(chat)    { tone, text, spinner?, dismissable? }
--                                     for the chat's composer (tracked)
--   sandbox_templates.dismiss(chat)   put that notice away
--   sandbox_templates.list(provider)  a promise of { { id, name } }
--
-- Each host call is a field (`ask_*`) a fixture can answer in the host's place.

local words = require("@app/screens/shell/save_template")

local M = {}

local KEPT = ui.shared("sandbox_templates", {} :: { [string]: any })
-- Chat → its last save (screens/shell/save_template's).
M.saves = ui.state("account_sandbox_template_saves", {} :: { [string]: any })

function M.ask_check(chats: { string })
	ui.invoke("sandbox_templates.check", chats)
end
function M.ask_save(chat: string, name: string): any
	return ui.call("sandbox_templates.save", { chat = chat, name = name })
end
function M.ask_list(provider: string): any
	return ui.call("sandbox_templates.list", provider)
end
-- (Read through here, so a fixture can stand in for the host's.)
function M.read(): { [string]: any }
	return KEPT:get() or {}
end

M.trimmed = words.trimmed

function M.check(chats: { string })
	if #chats > 0 then
		M.ask_check(chats)
	end
end

function M.can_save(chat: string?): boolean
	return chat ~= nil and M.read()[chat] ~= nil
end

function M.save(chat: string, name: string)
	if not words.can_start(M.saves:peek()[chat], name) then
		return
	end
	local save = words.saving(name)
	M.saves:set_key(chat, save)
	M.ask_save(chat, save.name):after(function(t: any)
		M.saves:set_key(chat, words.saved(save, t))
	end, function(e: any)
		M.saves:set_key(chat, words.failed(save, e))
	end)
end

function M.saving(chat: string?): boolean
	local s = chat and M.saves:get()[chat]
	return s ~= nil and s.state == "saving"
end

function M.notice(chat: string?): any
	return words.notice(chat and M.saves:get()[chat])
end

function M.dismiss(chat: string?)
	if chat and words.dismissable(M.saves:peek()[chat]) then
		M.saves:set_key(chat, nil :: any)
	end
end

function M.list(provider: string): any
	return M.ask_list(provider)
end

return M
