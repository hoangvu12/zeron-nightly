-- Save as sandbox template (spec #37, design 21–23): what's said of a chat's
-- save, as pure functions. account/sandbox_template_saves keeps the saves and
-- asks the host; this says what they mean. A test requires this module
-- alone, in a VM with no `ui` (it says the English there).
--
-- (A sandbox template is a provider's: what a sandbox starts from. Never
-- one of Zeron's own interface templates, `templates.*`.)
--
-- A save: { name, state = "saving" | "saved" | "failed", why? }.

local M = {}

-- Its words in the language shown; with no `ui` (a test), the English.
local function say(key: string, english: string, args: { [string]: any }?): string
	if (ui :: any) == nil then
		return (string.gsub(english, "{(%w+)}", function(k: string): string
			return tostring(args and args[k] or "")
		end))
	end
	return ui.t(`shell.sidebar.save_sandbox_template.{key}`, args):get()
end

-- The name as the engine saves it: trimmed.
function M.trimmed(name: string): string
	return string.match(name, "^%s*(.-)%s*$") or ""
end

-- Whether a Save may start: a name, and none already saving.
function M.can_start(save: any, name: string): boolean
	return M.trimmed(name) ~= "" and not (save and save.state == "saving")
end

function M.saving(name: string): any
	return { name = M.trimmed(name), state = "saving" }
end

-- Saved: under the name the provider gave it, else the one asked for.
function M.saved(save: any, template: any): any
	local name = if type(template) == "table" and type(template.name) == "string" then template.name else save.name
	return { name = name, state = "saved" }
end

-- Not saved: the engine's words, or ours when there are none (an outcome
-- word: offline, failed, outdated, signed_out).
function M.failed(save: any, e: any): any
	local m = if type(e) == "table" and type(e.message) == "string" then e.message else tostring(e)
	local plain = m == "offline" or m == "failed" or m == "outdated" or m == "signed_out" or m == ""
	return { name = save.name, state = "failed", why = if plain then nil else m }
end

-- The composer's band for it (ds/composer_band's Band), or nil: always
-- shown, never in the deck. Saving, the working ring and its name; saved
-- (screen 23), a green check, its label and the name in mono, put away with
-- ×; not saved, why.
function M.band(save: any): any
	if not save then
		return nil
	end
	if save.state == "saving" then
		return { kind = "saving", spinner = true, title = say("saving", "Saving as sandbox template"), detail = save.name, mono = true }
	elseif save.state == "saved" then
		return {
			kind = "saved",
			icon = "check-circle",
			tone = "success",
			title = say("saved", "Saved as sandbox template"),
			detail = save.name,
			mono = true,
			dismissable = true,
		}
	end
	return {
		kind = "save_failed",
		icon = "danger-triangle",
		tone = "danger",
		title = say("couldnt", "Couldn’t save the sandbox template"),
		detail = save.why,
		dismissable = true,
	}
end

-- Whether its band may be put away (not while it saves).
function M.dismissable(save: any): boolean
	return save ~= nil and save.state ~= "saving"
end

return M
