-- Save as template (spec #37, design 21–23): what's said of a chat's save,
-- as pure functions. account/sandbox_templates keeps the saves and asks the
-- host; this says what they mean. A test requires this module alone, in a VM
-- with no `ui` (it says the English there).
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
	return ui.t(`shell.sidebar.save_template.{key}`, args):get()
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

-- The composer's notice for it (ds/composer's Notice), or nil.
function M.notice(save: any): any
	if not save then
		return nil
	end
	if save.state == "saving" then
		return { tone = "info", spinner = true, text = say("saving", "Saving “{name}” as a template…", { name = save.name }) }
	elseif save.state == "saved" then
		return { tone = "success", dismissable = true, text = say("saved", "Saved as template {name}", { name = save.name }) }
	end
	return { tone = "danger", dismissable = true, text = save.why or say("couldnt", "Couldn’t save the template") }
end

-- Whether its notice may be put away (not while it saves).
function M.dismissable(save: any): boolean
	return save ~= nil and save.state ~= "saving"
end

return M
