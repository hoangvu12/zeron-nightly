-- A chat's sandbox, on its composer's deck: tracked reads and the action,
-- nothing drawn (as screens/chat/cloud_tray for Cloud). What's said is
-- screens/chat/sandbox_machine's; the composer follows the chat's steps
-- (account/sandboxes) while it's open.
--
--   local sandbox = SandboxTray.bind { chat = fn }
--   alerts: its alert, as the deck takes them; on_alert(id): its action

local sandboxes = require("@account/sandboxes")
local logic = require("@app/screens/chat/sandbox_machine")

local M = {}

export type Bind = {
	-- The open chat, while it runs on a sandbox; else nil.
	chat: () -> string?,
}

function M.bind(c: Bind): any
	local b: any = {}
	-- Its steps, with its provider's name when this device lists it.
	function b.steps(): any
		local s = sandboxes.steps_of(c.chat())
		if not s then
			return nil
		end
		local p = sandboxes.provider(s.provider)
		local out = table.clone(s) :: any
		out.provider_name = if p then p.name else nil
		return out
	end
	b.alert = ui.computed(function(): logic.Alert?
		return logic.alert(b.steps())
	end)
	b.alerts = ui.computed(function(): { any }
		local a = b.alert:get()
		if not a then
			return {}
		end
		local f = sandboxes.fresh_state(c.chat())
		return { logic.deck_alert(a, { busy = f.busy, failure = f.failure }) }
	end)
	-- Whether a deck id is its alert's.
	function b.owns(id: string): boolean
		return id == logic.ALERT_ID
	end
	function b.on_alert(_id: string)
		local chat, a = c.chat(), b.alert:peek()
		if chat and a and a.action == "fresh" then
			sandboxes.fresh(chat)
		end
	end
	return b
end

return M
