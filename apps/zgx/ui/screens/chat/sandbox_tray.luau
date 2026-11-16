-- A chat's sandbox, on its composer's deck: tracked reads and the actions,
-- nothing drawn (as screens/chat/cloud_tray for Cloud). What's said is
-- screens/chat/sandbox_machine's. The composer follows the chat's steps
-- (account/sandboxes) while it's open on a sandbox.
--
--   local sandbox = SandboxTray.bind { chat = fn }
--   alerts: its alert, as the deck takes them; on_alert(id): its action
--   line(): the deck's notice while it's asleep (moon, Wake); resting():
--   asleep or on its way to or from it (its host's offline, not unreachable)
--   wake(): Wake

local sandboxes = require("@account/sandboxes")
local logic = require("@app/screens/chat/sandbox_machine")

local M = {}

export type Bind = {
	-- The open chat while it runs on a sandbox (its device's row names a
	-- provider), else nil: a chat on any other device is never asked about.
	chat: () -> string?,
}

function M.bind(c: Bind): any
	local b: any = {}
	function b.steps(): any
		return sandboxes.machine(c.chat())
	end
	b.alert = ui.computed(function(): logic.Alert?
		return logic.alert(b.steps())
	end)
	b.alerts = ui.computed(function(): { any }
		local a = b.alert:get()
		if not a then
			return {}
		end
		local f = if a.action == "wake" then sandboxes.wake_state(c.chat()) else sandboxes.fresh_state(c.chat())
		return { logic.deck_alert(a, { busy = f.busy, failure = f.failure }) }
	end)
	function b.line(): any
		if sandboxes.wake_state(c.chat()).busy then
			-- (Woken: said in the transcript as it comes up.)
			return nil
		end
		return logic.line(b.steps())
	end
	function b.resting(): boolean
		return logic.resting(b.steps())
	end
	function b.wake()
		local chat = c.chat()
		if chat then
			sandboxes.wake(chat)
		end
	end
	-- Whether a deck id is its alert's.
	function b.owns(id: string): boolean
		return id == logic.ALERT_ID
	end
	function b.on_alert(_id: string)
		local chat, a = c.chat(), b.alert:peek()
		if not chat or not a then
			return
		end
		if a.action == "fresh" then
			sandboxes.fresh(chat)
		elseif a.action == "wake" then
			sandboxes.wake(chat)
		end
	end
	return b
end

return M
