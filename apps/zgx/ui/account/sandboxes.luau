-- Places a chat runs on (spec #37): the sandbox providers this device's
-- engine keeps (Docker Sandbox, Boat, …), as the host reads them
-- (src/sandboxes.rs). A sandbox's device is never listed: its provider is,
-- once, by its own name and icon (screens/chat/where). Picking it opens its
-- account's templates, plus Blank.
--
--   sandboxes.providers()           { { id, name, icon } } (tracked)
--   sandboxes.provider(id)          one of them, or nil (tracked)
--   sandboxes.icon(id)              its glyph: its own when Zeron draws it,
--                                   else a box (tracked)
--   sandboxes.name(id)              its name, else its id (tracked)
--   sandboxes.value(id) / .of(value)  the device picker's value for a provider
--                                   (`sandbox:<id>`), and back (nil: not one)
--   sandboxes.of_device(device)     the provider a sandbox's device row names
--   sandboxes.templates(provider)   a promise of { { id, name } }
--   sandboxes.follow(chat) / .unfollow(chat)  keep its steps in `steps`
--   sandboxes.steps_of(chat)        { state, step?, since, error? } (tracked)
--   sandboxes.run_again(chat)       a promise: the failed step runs again
--
-- The host's calls are fields (`ask_*`), so a fixture answers in its place.

local SVGS: { [string]: string } = require("@ds/icon_svgs")

local M = {}

export type Provider = { id: string, name: string, icon: string }
export type Template = { id: string, name: string }
export type Steps = { state: string, step: string?, template: string?, since: number, error: string? }

M.PREFIX = "sandbox:"
-- What a template picker's Blank is (no template: the provider's plain image).
M.BLANK = ""

M.now = ui.shared("sandboxes", { providers = {} :: { Provider } })
M.steps = ui.shared("sandbox_steps", {} :: { [string]: Steps })

function M.providers(): { Provider }
	return M.now:get().providers or {}
end

function M.provider(id: string?): Provider?
	if not id then
		return nil
	end
	for _, p in M.providers() do
		if p.id == id then
			return p
		end
	end
	return nil
end

-- A provider's glyph: the one it names when Zeron's set draws it (same
-- grid, stroke and corners as every other), else a box.
function M.icon(id: string?): string
	local p = M.provider(id)
	local icon = if p then p.icon else id
	return if icon and SVGS[icon] then icon else "box"
end

function M.name(id: string): string
	local p = M.provider(id)
	return if p then p.name else id
end

function M.value(id: string): string
	return M.PREFIX .. id
end

function M.of(value: string?): string?
	if value and string.sub(value, 1, #M.PREFIX) == M.PREFIX and #value > #M.PREFIX then
		return string.sub(value, #M.PREFIX + 1)
	end
	return nil
end

function M.of_device(device: { sandbox: string? }?): string?
	return if device then device.sandbox else nil
end

function M.steps_of(chat: string?): Steps?
	if not chat then
		return nil
	end
	return M.steps:get()[chat]
end

-- The host's calls, each a field a fixture answers.
function M.ask_templates(provider: string): any
	return ui.call("sandboxes.templates", provider)
end
function M.ask_run_again(chat: string): any
	return ui.call("sandboxes.run_again", chat)
end
function M.ask_follow(chat: string)
	ui.invoke("sandboxes.follow", chat)
end
function M.ask_unfollow(chat: string)
	ui.invoke("sandboxes.unfollow", chat)
end

function M.templates(provider: string): any
	return M.ask_templates(provider)
end
function M.run_again(chat: string): any
	return M.ask_run_again(chat)
end
function M.follow(chat: string)
	M.ask_follow(chat)
end
function M.unfollow(chat: string)
	M.ask_unfollow(chat)
end

return M
