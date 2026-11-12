-- The Design tab (screens/panel/design) mounted on its own: a chat's panel
-- holding one Design tab on `fixture.zdesign`, its Canvas drawn by the real
-- host (src/design.rs) from what a stand-in engine says
-- (src/design_tests.rs). Commands open a second, empty Design tab beside
-- it (`design_empty`) and switch between them; `design_fit` views the
-- whole Design again (once the test has sized the window).

ui.theme.strict("warn")
local ds = require("@ds")
local toast = require("@ds/toast")
ds.setup()
local state = require("@app/screens/panel/state")
local Design = require("@app/screens/panel/design")

local KEY = "chat:design-fixture"
local PATH = "fixture.zdesign"

-- Its tab, made once (a reload finds it).
if #state.of(KEY).tabs == 0 then
	state.open(KEY, "design", { path = PATH })
end

ui.command {
	id = "design_empty",
	title = "A Design tab with no Design yet",
	run = function()
		state.open(KEY, "design")
	end,
}
ui.command {
	id = "design_fit",
	title = "The Design tab's view on all of its Design",
	run = function()
		local active = state.of(KEY).active
		if active then
			Design.fit(active)
		end
	end,
}
ui.command {
	id = "design_first",
	title = "Back to the first Design tab",
	run = function()
		state.activate(KEY, state.of(KEY).tabs[1].id)
	end,
}

return ui.column {
	w_full = true,
	h_full = true,
	ui.presence(function(): string
		return state.of(KEY).active or ""
	end, function(id: string): Node
		if id == "" then
			return ui.column {}
		end
		return Design.Design { key = KEY, tab = id }
	end, { w_full = true, grow = 1, min_h = 0 }),
	-- (Where what failed is said, as the window shows it.)
	toast.Host {},
}
