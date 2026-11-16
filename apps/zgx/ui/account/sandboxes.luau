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
--                                   (`place:<id>`), and back (nil: not one)
--   sandboxes.target(id, template?) the agents' catalog target for a place
--                                   (`place:<id>[:<template>]`, zeron_proto
--                                   `places::place_target`): the device keeping
--                                   the provider answers, never a sandbox
--   sandboxes.of_device(device)     the provider a sandbox's device row names
--   sandboxes.templates(provider)   a promise of { { id, name } }
--   sandboxes.follow(chat) / .unfollow(chat)  keep its steps in `steps`
--   sandboxes.steps_of(chat)        { state, step?, since, error?, provider?,
--                                   disk_full } (tracked); `state` also
--                                   "gone": deleted outside Zeron
--   sandboxes.run_again(chat)       a promise: the failed step runs again
--   sandboxes.fresh(chat)           Start fresh: a new sandbox, the chat's
--                                   pushed work with it
--   sandboxes.fresh_state(chat)     { busy?, failure? } of the last Start
--                                   fresh (tracked)
--
-- The host's calls are fields (`ask_*`), so a fixture answers in its place.

local SVGS: { [string]: string } = require("@ds/icon_svgs")

local M = {}

export type Provider = { id: string, name: string, icon: string }
export type Template = { id: string, name: string }
export type Steps = {
	state: string,
	step: string?,
	template: string?,
	since: number,
	error: string?,
	provider: string?,
	disk_full: boolean?,
}

-- (A place's own name for the engine, as zeron_proto `places` has it; not a
-- device's `sandbox:<provider>` capability.)
M.PREFIX = "place:"
-- What a template picker's Blank is (no template: the provider's plain image).
M.BLANK = ""

M.now = ui.shared("sandboxes", { providers = {} :: { Provider } })
M.steps = ui.shared("sandbox_steps", {} :: { [string]: Steps })
M.fresh_states = ui.state("account_sandboxes_fresh", {} :: { [string]: { busy: boolean?, failure: string? } })

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

-- The provider a picker value or catalog target names (a template after it
-- dropped), else nil.
function M.of(value: string?): string?
	if value and string.sub(value, 1, #M.PREFIX) == M.PREFIX and #value > #M.PREFIX then
		local rest = string.sub(value, #M.PREFIX + 1)
		return string.match(rest, "^([^:]+)") or rest
	end
	return nil
end

function M.target(id: string, template: string?): string
	if template and template ~= M.BLANK then
		return `{M.PREFIX}{id}:{template}`
	end
	return M.PREFIX .. id
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
function M.ask_fresh(chat: string): any
	return ui.call("sandboxes.fresh", chat)
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

function M.fresh_state(chat: string?): { busy: boolean?, failure: string? }
	return (chat and M.fresh_states:get()[chat]) or {}
end

-- Start fresh, once at a time per chat; why it didn't go kept for the deck.
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
