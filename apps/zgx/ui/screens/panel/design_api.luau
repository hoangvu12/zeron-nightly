-- A Design tab's host (src/design.rs), behind one module so the Luau
-- fixtures can stand in for it (ui/panel_home_fixture.luau,
-- ui/panel_fixture.luau); the Design tab's own fixture runs the real host.
--
--   api.invoke(name, arg)    a host action (`design.open`, `design.view`…)
--   api.call(name, arg)      a host call, a promise (`design.new`, `design.save`…)
--   api.call_sync(name, arg) a host function's answer now (`design.hit`)

local M = {}

function M.invoke(name: string, arg: any)
	ui.invoke(name, arg)
end

function M.call(name: string, arg: any): any
	return ui.call(name, arg)
end

function M.call_sync(name: string, arg: any): any
	return ui.call_sync(name, arg)
end

return M
