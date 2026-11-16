-- Save as template (spec #37): a chat's sandbox saved through its provider,
-- by the engine that keeps it (src/sandbox_templates.rs). Named apart from
-- the interface's own templates (`templates.*`).
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

local locale = require("@ds/locale")

local M = {}

local KEPT = ui.shared("sandbox_templates", {} :: { [string]: any })
-- Chat → its last save: { name, state = "saving" | "saved" | "failed", why? }.
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

function M.check(chats: { string })
	if #chats > 0 then
		M.ask_check(chats)
	end
end

function M.can_save(chat: string?): boolean
	return chat ~= nil and M.read()[chat] ~= nil
end

-- The engine's words, or what an outcome word means.
local function why(e: any): string
	local m = if type(e) == "table" and type(e.message) == "string" then e.message else tostring(e)
	if m == "offline" or m == "failed" or m == "outdated" or m == "signed_out" or m == "" then
		return locale.s("shell.sidebar.save_template.couldnt")
	end
	return m
end

-- The name as the engine saves it: trimmed.
function M.trimmed(name: string): string
	return string.match(name, "^%s*(.-)%s*$") or ""
end

function M.save(chat: string, name: string)
	local now = M.saves:peek()[chat]
	if now and now.state == "saving" then
		return
	end
	name = M.trimmed(name)
	if name == "" then
		return
	end
	M.saves:set_key(chat, { name = name, state = "saving" } :: any)
	M.ask_save(chat, name):after(function(t: any)
		local saved = if type(t) == "table" and type(t.name) == "string" then t.name else name
		M.saves:set_key(chat, { name = saved, state = "saved" } :: any)
	end, function(e: any)
		M.saves:set_key(chat, { name = name, state = "failed", why = why(e) } :: any)
	end)
end

function M.saving(chat: string?): boolean
	local s = chat and M.saves:get()[chat]
	return s ~= nil and s.state == "saving"
end

function M.notice(chat: string?): any
	local s = chat and M.saves:get()[chat]
	if not s then
		return nil
	end
	if s.state == "saving" then
		return { tone = "info", spinner = true, text = locale.s("shell.sidebar.save_template.saving", { name = s.name }) }
	elseif s.state == "saved" then
		return { tone = "success", dismissable = true, text = locale.s("shell.sidebar.save_template.saved", { name = s.name }) }
	end
	return { tone = "danger", dismissable = true, text = s.why or locale.s("shell.sidebar.save_template.couldnt") }
end

function M.dismiss(chat: string?)
	local s = chat and M.saves:peek()[chat]
	if s and s.state ~= "saving" then
		M.saves:set_key(chat :: string, nil :: any)
	end
end

function M.list(provider: string): any
	return M.ask_list(provider)
end

return M
