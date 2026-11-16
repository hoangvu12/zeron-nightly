-- What's said of a chat's sandbox when something went wrong with it (spec #37
-- "When things go wrong"), and what can be done: pure functions over its
-- steps (account/sandboxes), nothing drawn. screens/chat/sandbox_tray puts it
-- on the composer's deck.
--
--   alert(steps)                 the one thing to raise over the composer:
--                                > deleted outside Zeron ("Sandbox deleted in
--                                  Docker", Start fresh)
--                                > its disk full (Start fresh)
--                                > a wake that failed (Wake)
--   line(steps)                  the deck's notice: asleep (moon, Wake)
--   resting(steps)               asleep, or going to or from it
--   deck_alert(alert, opts)      that alert as ds/composer_dock's Alert
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

-- The deck's id for it, beside Cloud's.
M.ALERT_ID = "\0sandbox"

export type Alert = {
	-- gone, disk.
	kind: string,
	tone: string,
	title: string,
	-- The folded line.
	summary: string,
	-- The opened words.
	body: string,
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
			summary = say("gone_summary", "The conversation is kept"),
			body = say(
				"gone_body",
				"This chat's sandbox was deleted outside Zeron. The conversation is kept. Start fresh makes a new sandbox from its repository, on the chat's branch: what was pushed comes back, what wasn't is lost."
			),
			action = "fresh",
			action_label = fresh,
		}
	end
	if steps.state == "failed" and steps.step == "wake" then
		return {
			kind = "wake_failed",
			tone = "danger",
			title = say("wake_failed_title", "Sandbox didn't wake"),
			summary = say("wake_failed_summary", "Try waking it again"),
			body = if type(steps.error) == "string" and steps.error ~= ""
				then steps.error
				else say("wake_failed_body", "This chat's sandbox didn't wake. Try again in a moment."),
			action = "wake",
			action_label = say("wake", "Wake"),
		}
	end
	if steps.disk_full then
		return {
			kind = "disk",
			tone = "danger",
			title = say("disk_title", "Sandbox's disk is full"),
			summary = say("disk_summary", "Free some space, or start fresh"),
			body = if type(steps.error) == "string" and steps.error ~= ""
				then steps.error
				else say("disk_body", "This chat's sandbox ran out of disk. Free some space, or start fresh: a new sandbox with what was pushed."),
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

-- The alert as ds/composer_dock's Alert: plain data. `busy`: Start fresh is on
-- its way. `failure`: why the last one didn't go.
function M.deck_alert(a: Alert, opts: { busy: boolean?, failure: string? }?): { [string]: any }
	local o = opts or {}
	return {
		id = M.ALERT_ID,
		tone = a.tone,
		title = a.title,
		detail = if a.summary ~= a.body then a.summary else nil,
		body = a.body,
		note = if o.failure then M.failure_words(o.failure, a.action) else nil,
		action = if o.busy then nil else a.action_label,
		spinner = o.busy == true,
		dismissable = false,
	}
end

return M
