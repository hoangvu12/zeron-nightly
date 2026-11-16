-- A chat's sandbox, in its composer's band (a row of its own over the
-- composer, always shown, never in the deck's alerts): tracked reads and the
-- actions, nothing drawn (as screens/chat/cloud_tray for Cloud). What's said
-- is screens/chat/sandbox_machine's. The composer follows the chat's steps
-- (account/sandboxes) while it's open on a sandbox.
--
--   local sandbox = SandboxTray.bind { chat = fn }
--   band(): what's wrong with it (deleted, its disk full, a wake that
--   failed), else asleep (moon, Wake), as the band takes it; nil for none
--   act(): its band's action (Start fresh, Wake)
--   resting(): asleep or on its way to or from it (its host's offline, not
--   unreachable)
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
	function b.band(): { [string]: any }?
		local a = b.alert:get()
		if a then
			local f = if a.action == "wake" then sandboxes.wake_state(c.chat()) else sandboxes.fresh_state(c.chat())
			local s = b.steps()
			return logic.band(a, { busy = f.busy, failure = f.failure, icon = sandboxes.icon(s and s.provider) })
		end
		local line = b.line()
		return if line then logic.asleep_band(line) else nil
	end
	function b.line(): any
		if sandboxes.wake_state(c.chat()).busy then
			-- (Woken: its band says so as it comes up.)
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
	function b.act()
		local chat, a = c.chat(), b.alert:peek()
		if not chat then
			return
		end
		if not a then
			-- Asleep: Wake.
			sandboxes.wake(chat)
		elseif a.action == "fresh" then
			sandboxes.fresh(chat)
		elseif a.action == "wake" then
			sandboxes.wake(chat)
		end
	end
	return b
end

return M
