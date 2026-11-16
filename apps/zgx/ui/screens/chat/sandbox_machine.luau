-- What's said of a chat's sandbox when something went wrong with it (spec #37
-- "When things go wrong"), and what can be done: pure functions over its
-- steps (account/sandboxes), nothing drawn. screens/chat/sandbox_tray puts it
-- in the composer's band, a row of its own over it.
--
--   alert(steps)                 the one thing to raise over the composer:
--                                > deleted outside Zeron ("Sandbox deleted in
--                                  Docker", Start fresh)
--                                > its disk full (Start fresh)
--                                > a wake that failed (Wake)
--   line(steps)                  asleep (moon, Wake)
--   resting(steps)               asleep, or going to or from it
--   band(alert, opts)            that alert as the composer's band, its row of
--                                its own over the composer (ds/composer_band)
--   asleep_band(line)            asleep, the same
--   failure_words(word)          why Start fresh didn't go
--
-- A test requires this module alone, in a VM with no `ui`: there, the English
-- it says (as screens/chat/cloud_machine).

local M = {}

local function say(key: string, english: string, args: { [string]: any }?): string
	if (ui :: any) == nil then
		return english
	end
	return ui.t(`chat.sandbox_machine.{key}`, args):get()
end

export type Alert = {
	-- gone, disk, wake_failed.
	kind: string,
	tone: string,
	title: string,
	-- A word more, muted after it (none for deleted: its title and Start
	-- fresh say it, screen 14).
	detail: string?,
	-- "fresh" or "wake".
	action: string?,
	action_label: string?,
}

-- The provider as a place is named: "Docker" for "Docker Sandbox".
function M.place_name(steps: any): string?
	local name = steps.provider_name
	if type(name) ~= "string" or name == "" then
		return nil
	end
	local short = string.gsub(name, "%s+[Ss]andbox$", "")
	return if short ~= "" then short else name
end

function M.alert(steps: any?): Alert?
	if not steps then
		return nil
	end
	local fresh = say("fresh", "Start fresh")
	if steps.state == "gone" then
		local place = M.place_name(steps)
		return {
			kind = "gone",
			tone = "warning",
			title = if place
				then say("gone_title_in", `Sandbox deleted in {place}`, { place = place })
				else say("gone_title", "Sandbox deleted"),
			action = "fresh",
			action_label = fresh,
		}
	end
	if steps.state == "failed" and steps.step == "wake" then
		return {
			kind = "wake_failed",
			tone = "danger",
			title = say("wake_failed_title", "Sandbox didn't wake"),
			detail = say("wake_failed_summary", "Try waking it again"),
			action = "wake",
			action_label = say("wake", "Wake"),
		}
	end
	if steps.disk_full then
		return {
			kind = "disk",
			tone = "danger",
			title = say("disk_title", "Sandbox's disk is full"),
			detail = say("disk_summary", "Free some space, or start fresh"),
			action = "fresh",
			action_label = fresh,
		}
	end
	return nil
end

-- Whether it's stopped, or on its way to or from it: its host device is
-- offline then, and that isn't "Can't reach" (link_notice).
function M.resting(steps: any?): boolean
	local s = steps and steps.state
	return s == "sleeping" or s == "asleep" or s == "waking"
end

-- The deck's notice for a sandbox asleep (spec #37 "Sleep and wake"): the
-- moon, and Wake. Nil otherwise: going to sleep or waking is said in the
-- transcript, where a send waits for it, and a failed wake is an alert.
function M.line(steps: any?): { [string]: any }?
	if not steps or steps.state ~= "asleep" then
		return nil
	end
	return { text = say("asleep", "Sandbox asleep"), action = say("wake", "Wake"), icon = "moon" }
end

-- Why Start fresh (or Wake, `action` "wake") didn't go, in words.
function M.failure_words(word: string, action: string?): string
	local wake = action == "wake"
	if word == "offline" then
		return say("failure_offline", "Couldn't reach Zeron on this device. Try again once it's back.")
	elseif word == "outdated" then
		return if wake
			then say("wake_failure_outdated", "This device's Zeron is too old to wake a sandbox. Update it, then try again.")
			else say("failure_outdated", "This device's Zeron is too old to start a sandbox fresh. Update it, then try again.")
	elseif word == "signed_out" then
		return say("failure_signed_out", "Sign in again, then try again.")
	end
	return if wake then say("wake_failure", "Couldn't wake it. Try again.") else say("failure", "Couldn't start fresh. Try again.")
end

-- The alert as the composer's band (screens/chat/sandbox_line's Band): one
-- row, its title and action alone (screen 14). Deleted wears its provider's
-- glyph (`icon`, Docker's), the rest their tone's. `busy`: Start fresh (or
-- Wake) is on its way, the working ring in its glyph's place. `failure`: why
-- the last one didn't go, said after the title.
function M.band(a: Alert, opts: { busy: boolean?, failure: string?, icon: string? }?): { [string]: any }
	local o = opts or {}
	return {
		kind = a.kind,
		tone = if a.kind == "gone" then nil else a.tone,
		icon = if a.kind == "gone" then (o.icon or "box") else "danger-triangle",
		spinner = o.busy == true,
		title = a.title,
		detail = if o.failure then M.failure_words(o.failure, a.action) else a.detail,
		action = if o.busy then nil else a.action_label,
	}
end

-- Asleep as the band (screen 08): the moon, and Wake; always shown.
function M.asleep_band(line: { [string]: any }): { [string]: any }
	return { kind = "asleep", icon = line.icon, title = line.text, action = line.action }
end

return M
