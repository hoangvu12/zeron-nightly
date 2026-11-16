-- A chat's panel tab that reaches its sandbox (Files, Terminal, Diffs), while
-- that sandbox is asleep (spec #37 "Sleep and wake"): the moon, "Sandbox
-- asleep", and Wake, in the tab's place (no line under it, design 08). Opening the tab never wakes it;
-- awake again, the tab is built as it was.
--
--   Asleep.gate(chat, function() return Files.Tab { … } end)

local ds = require("@ds")
local sandboxes = require("@account/sandboxes")
local logic = require("@app/screens/chat/sandbox_machine")

local M = {}

-- Whether `chat`'s tab waits for its sandbox: asleep, or on its way to or
-- from it.
function M.waits(chat: string?): boolean
	return logic.resting(sandboxes.machine(chat))
end

function M.gate(chat: string?, build: () -> Node): Node
	if not chat then
		return build()
	end
	return ui.presence(function(): string
		return if M.waits(chat) then "asleep" else "awake"
	end, function(key: string): Node
		if key == "awake" then
			return build()
		end
		return ds.EmptyState {
			icon = "moon",
			size = "sm",
			fill = true,
			-- (Design 08: the moon, its title and Wake; no line under it. On
			-- its way to or from sleep, the title says so instead.)
			title = function(): string
				local s = sandboxes.machine(chat)
				local busy = sandboxes.wake_state(chat).busy
				if busy or (s and s.state == "waking") then
					return ui.t("panel.asleep.waking"):get()
				elseif s and s.state == "sleeping" then
					return ui.t("panel.asleep.sleeping"):get()
				end
				return ui.t("panel.asleep.title"):get()
			end,
			action = {
				label = ui.t("panel.asleep.wake") :: any,
				on_click = function()
					sandboxes.wake(chat)
				end,
			},
		}
	end, { w_full = true, grow = 1, min_h = 0 })
end

return M
