-- Save as template (spec #37): a chat's sandbox saved through its provider,
-- by the host device that keeps it (src/sandbox_templates.rs). Named apart
-- from the interface's own templates (`templates.*`). What a save says is
-- screens/shell/save_template's; a provider's templates are listed by
-- account/sandboxes.
--
--   sandbox_templates.on_sandbox(chat)  whether its device is a sandbox
--                                     (tracked): its menu offers the save
--   sandbox_templates.save(chat, name)  saves it; while it does, and once
--                                     it's done, its `notice(chat)` says so
--   sandbox_templates.saving(chat)    whether it's saving (tracked)
--   sandbox_templates.notice(chat)    { tone, text, spinner?, dismissable? }
--                                     for the chat's composer (tracked)
--   sandbox_templates.dismiss(chat)   put that notice away
--
-- The host's call is a field (`ask_save`) a fixture can answer in its place.

local sessions = require("@account/sessions")
local sandboxes = require("@account/sandboxes")
local words = require("@app/screens/shell/save_template")

local M = {}

-- Chat → its last save (screens/shell/save_template's).
M.saves = ui.state("account_sandbox_template_saves", {} :: { [string]: any })

function M.ask_save(chat: string, name: string): any
	return ui.call("sandbox_templates.save", { chat = chat, name = name })
end

M.trimmed = words.trimmed

function M.on_sandbox(chat: string?): boolean
	if not chat then
		return false
	end
	local device: string? = nil
	for _, c in sessions.now:get().chats do
		if c.id == chat then
			device = c.device
			break
		end
	end
	if not device then
		return false
	end
	for _, d in sessions.devices:get() do
		if d.id == device then
			return sandboxes.of_device(d :: any) ~= nil
		end
	end
	return false
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

return M
