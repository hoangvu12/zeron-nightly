-- A Design tab: one Design's Canvas (docs/design/GLOSSARY.md), live. The
-- host (src/design.rs) follows the engine's Design and draws it into
-- tiles, the same drawing as the agent's screenshots; this places them and
-- moves them as the Canvas is panned and zoomed (the wheel pans, ⌘ or Ctrl
-- with it and a pinch zoom, from 0.25x to 5x, and a drag pans), so the
-- Canvas moves at once and the host fills in sharper tiles behind it.
-- Over the tiles, the live touches: what an agent just changed flashes,
-- and a Screen it marked in progress looks so until it clears the mark.
--
-- The view (its pan and zoom) is kept per tab here, outside the views, so
-- switching tabs or a reload finds it where it was. A tab with no Design
-- yet offers New design (in Zeron's designs folder), Recent designs (those
-- touched on this computer, by agents too) and Open design (a .zdesign
-- anywhere); nothing opens one on its own. Saving is the engine's (#28): a
-- Design in Zeron's folder saves itself; one of your own is written on ⌘S
-- (⌘⇧S saves it as another file), its changes kept meanwhile and offered
-- back after a crash as Recovered changes.
--
--   Design.Design { key, tab }
--   Design.title(tab)     the Design's name
--   Design.dirty(tab)     whether it has changes its file doesn't
--   Design.save(tab, done?)     its file written now (⌘S)
--   Design.discard(tab, done?)  back as its file holds it
--   Design.close(tab)     its tiles let go
--   Design.fit(id)        the tab's view on all of its Design again
--   Design.Empty { title, body, action? }   what shows in place of a Canvas
--   Design.say_tabs(key)  the engine told which Designs the chat's tabs show

local ds = require("@ds")
local motion = require("@ds/motion")
local state = require("@app/screens/panel/state")
local api = require("@app/screens/panel/design_api")

type Tab = state.Tab

local M = {}

local ZOOM_MIN, ZOOM_MAX = 0.25, 5
-- How long after the last wheel step a zoom is over, and its tiles drawn
-- at its own level.
local SETTLE_MS = 150
-- How long what an agent just changed flashes (pen.dev's live touches).
local FLASH_MS = 900

-- Each tab's view: where the Canvas's origin is in it, and its zoom.
type View = { x: number, y: number, zoom: number }
local views = ui.state("design_views", {} :: { [string]: View })
-- The flashes already shown (`tab:version:node`), kept across reloads.
local flashed = ui.state("design_flashed", {} :: { [string]: boolean })

-- What the host says of a tab's Canvas (src/design.rs): its state, where
-- its Design draws and its tiles' layers.
local function shown(id: string): State<any>
	return ui.shared(`design.tab.{id}`, nil :: any)
end

-- Its Design's name: its file's, as the host follows it (the tab's own
-- record may be from before it had one, or before Save as).
function M.title(tab: Tab): string?
	local b = shown(tab.id):get()
	local path = if b and type(b.path) == "string" and b.path ~= "" then b.path else tab.path
	if not path then
		return nil
	end
	return string.match(path, "([^/\\]+)%.zdesign$") or string.match(path, "([^/\\]+)$")
end

function M.dirty(tab: Tab): boolean
	local b = shown(tab.id):get()
	return b ~= nil and b.unsaved == true
end

-- (What fails is said by the engine's words, in the window's log: the tab
-- keeps its marker, so nothing is lost by it.)
local function said(what: string): (any) -> ()
	return function(err: any)
		print(`zeron-zgx: design: {what}: {if type(err) == "table" and err.message then err.message else tostring(err)}`)
	end
end

-- (`done` once the engine has: a close waits for it.)
function M.save(tab: Tab, done: (() -> ())?)
	api.call("design.save", { tab = tab.id }):after(function()
		if done then
			done()
		end
	end, said("save"))
end

function M.discard(tab: Tab, done: (() -> ())?)
	api.call("design.discard", { tab = tab.id }):after(function()
		if done then
			done()
		end
	end, said("discard"))
end

-- The Design tab showing now, and how to save it as another file (⌘S and
-- ⌘⇧S act on it; one panel shows at a time).
local front: { tab: Tab, save_as: () -> () }? = nil

local function design_filter(): { any }
	return { { ui.t("panel.design.filter"):get(), { "zdesign" } } }
end

ui.command {
	id = "panel.design.save",
	title = ui.t("panel.design.command.save"):get(),
	menu = "File",
	key = "cmd+s",
	run = function(): boolean
		if not front then
			return false
		end
		M.save(front.tab)
		return true
	end,
}
ui.command {
	id = "panel.design.save_as",
	title = ui.t("panel.design.command.save_as"):get(),
	menu = "File",
	key = "cmd+shift+s",
	run = function(): boolean
		if not front then
			return false
		end
		front.save_as()
		return true
	end,
}

function M.close(tab: Tab)
	api.invoke("design.close", tab.id)
	views:set_key(tab.id, nil)
end

function M.fit(id: string)
	views:set_key(id, nil)
end

-- Tells the engine which Designs a chat's Design tabs show, the one turned
-- to last first (what its agent's calls naming no Design go by). The host
-- says it only when it changed, and again to a new engine.
function M.say_tabs(key: string)
	local chat = string.match(key, "^chat:(.+)$")
	if not chat then
		return
	end
	local s = state.of(key)
	local paths: { string } = {}
	local function add(t: Tab)
		if t.kind == "design" and t.path and not table.find(paths, t.path) then
			table.insert(paths, t.path)
		end
	end
	for _, seen in s.visited do
		local t = state.find(key, seen)
		if t then
			add(t)
		end
	end
	for _, t in s.tabs do
		add(t)
	end
	api.invoke("design.tabs", { chat = chat, paths = paths })
end

-- What a Design tab shows instead of a Canvas: its kind's empty state.
function M.Empty(props: { title: any, body: any, action: any? }): Node
	return ds.EmptyState {
		icon = "palette",
		size = "sm",
		fill = true,
		title = props.title,
		body = props.body,
		action = props.action,
	}
end

-- A tab's Design, once it has one, as the host draws it.
local function canvas(id: string): Node
	local host = shown(id)
	local function told(): any
		return host:get()
	end
	local function view(): View?
		return views:get()[id]
	end
	local size = ui.use_state({ w = 0, h = 0 })
	local settled = ui.use_state(true)
	local zooms = 0
	local pointer: { x: number, y: number }? = nil

	-- The host told what shows, as it changes.
	ui.effect(function()
		local v, s = view(), size:get()
		if not v or s.w <= 0 then
			return
		end
		api.invoke("design.view", { tab = id, x = v.x, y = v.y, zoom = v.zoom, w = s.w, h = s.h, settled = settled:get() })
	end)
	-- A Design seen for the first time: all of it in view, at most at 1x.
	ui.effect(function()
		local b, s = told(), size:get()
		if view() or not b or b.state ~= "ready" or s.w <= 0 then
			return
		end
		local e = b.extent
		local zoom = 1
		if e and e.w > 0 and e.h > 0 then
			zoom = math.clamp(math.min(s.w * 0.9 / e.w, s.h * 0.9 / e.h, 1), ZOOM_MIN, ZOOM_MAX)
		end
		local cx = if e then e.x + e.w / 2 else 0
		local cy = if e then e.y + e.h / 2 else 0
		views:set_key(id, { x = s.w / 2 - cx * zoom, y = s.h / 2 - cy * zoom, zoom = zoom })
	end)

	local function pan(dx: number, dy: number)
		local v = views:peek()[id]
		if v then
			views:set_key(id, { x = v.x + dx, y = v.y + dy, zoom = v.zoom })
		end
	end
	-- Zoomed by `factor` about (`at_x`, `at_y`) in the tab, the point under
	-- it staying put; settled a moment after the last step.
	local function zoom_at(factor: number, at_x: number, at_y: number)
		local v = views:peek()[id]
		if not v then
			return
		end
		local zoom = math.clamp(v.zoom * factor, ZOOM_MIN, ZOOM_MAX)
		local wx, wy = (at_x - v.x) / v.zoom, (at_y - v.y) / v.zoom
		ui.batch(function()
			settled:set(false)
			views:set_key(id, { x = at_x - wx * zoom, y = at_y - wy * zoom, zoom = zoom })
		end)
		zooms += 1
		local mine = zooms
		ui.after(SETTLE_MS, function()
			if zooms == mine then
				settled:set(true)
			end
		end)
	end

	-- A level's tiles, placed in points at its level and scaled to the zoom.
	local function layer(_first: any, _i: number, now: State<any>): Node
		local function scale(): number
			local v = view()
			return if v then v.zoom / now:get().level else 1
		end
		return ui.div {
			class = "design-layer",
			absolute = true,
			left = 0,
			top = 0,
			w = 1,
			h = 1,
			origin = { 0, 0 },
			x = function(): number
				local v = view()
				return if v then v.x else 0
			end,
			y = function(): number
				local v = view()
				return if v then v.y else 0
			end,
			scale = scale,
			ui.each(ui.computed(function(): { any }
				return now:get().tiles
			end), function(t: any): Node
				return ui.image {
					id = t.id,
					class = "design-tile",
					src = t.src,
					absolute = true,
					left = t.x,
					top = t.y,
					w = t.size,
					h = t.size,
				}
			end, {
				key = function(t: any): string
					return t.src
				end,
			}),
		}
	end

	-- A box over a node of the Canvas, in the tab's points: where the view
	-- puts it, crisp at any zoom (not scaled with the tiles).
	local function over(n: any): { [string]: any }
		return {
			absolute = true,
			left = function(): number
				local v = view()
				return if v then v.x + n.x * v.zoom else 0
			end,
			top = function(): number
				local v = view()
				return if v then v.y + n.y * v.zoom else 0
			end,
			w = function(): number
				local v = view()
				return if v then n.w * v.zoom else 0
			end,
			h = function(): number
				local v = view()
				return if v then n.h * v.zoom else 0
			end,
		}
	end
	-- What an agent just changed: lit as it comes, fading out; then the
	-- host's told it's over (a reload doesn't flash it again).
	local function flash(n: any): Node
		local once = `{id}:{n.version}:{n.id}`
		-- (Lit only the first time it's built: rebuilt by a reload while it
		-- fades, it stays out.)
		local lit = ui.use_state(not flashed:peek()[once])
		ui.effect(function()
			if flashed:peek()[once] then
				return
			end
			flashed:set_key(once, true)
			ui.after(0, function()
				lit:set(false)
			end)
			ui.after(FLASH_MS, function()
				-- (Over: the host lets it go, so a reload won't build it.)
				api.invoke("design.flashed", { tab = id, version = n.version })
				flashed:set_key(once, nil)
			end)
		end)
		local box = over(n)
		box.id = `design-flash-{id}-{n.id}`
		box.class = "design-flash"
		box.aria_hidden = true
		box.border = 2
		box.border_color = "$accent"
		box.bg = "$selection.item"
		box.opacity = motion.tween(ui.computed(function(): number
			return if lit:get() then 1 else 0
		end), { duration = FLASH_MS, ease = "outQuad" })
		return ui.div(box)
	end
	-- A Screen an agent marked in progress: outlined, veiled, and saying so
	-- over its corner, until the agent clears the mark.
	local function working(n: any): Node
		local box = over(n)
		box.id = `design-working-{id}-{n.id}`
		box.class = "design-working"
		box.border = 2
		box.border_color = "$accent"
		box.bg = "$hover"
		box[1] = ui.div {
			absolute = true,
			left = 0,
			bottom = "100%",
			mb = "$space.1",
			ds.Badge { ui.t("panel.design.in_progress"), tone = "accent", look = "solid", shape = "pill" },
		}
		return ui.div(box)
	end
	local function touches(kind: string): any
		return ui.computed(function(): { any }
			local b = told()
			return if b and b[kind] then b[kind] else {}
		end)
	end

	return ui.overlay {
		id = `design-canvas-{id}`,
		class = "design-canvas",
		role = "group",
		label = ui.t("panel.design.label"),
		w_full = true,
		grow = 1,
		min_h = 0,
		overflow = "hidden",
		relative = true,
		bg = "$surface.sunken",
		on_resize = function(s: { w: number, h: number })
			local now = size:peek()
			if now.w ~= s.w or now.h ~= s.h then
				size:set({ w = s.w, h = s.h })
			end
		end,
		on_wheel = function(e: any): boolean
			if e.ctrl or e.meta then
				zoom_at(math.exp(-e.dy * 0.01), e.local_x, e.local_y)
			else
				pan(-e.dx, -e.dy)
			end
			return true
		end,
		on_pinch = function(e: any): boolean
			zoom_at(1 + e.scale, e.local_x, e.local_y)
			return true
		end,
		on_press = function(e: any)
			pointer = { x = e.local_x, y = e.local_y }
		end,
		on_pointer_move = function(e: any)
			local p = pointer
			if p then
				pan(e.local_x - p.x, e.local_y - p.y)
				pointer = { x = e.local_x, y = e.local_y }
			end
		end,
		on_release = function(_e: any)
			pointer = nil
		end,
		ui.each(ui.computed(function(): { any }
			local b = told()
			return if b then b.layers else {}
		end), layer, {
			key = function(l: any): string
				return tostring(l.level)
			end,
		}),
		ui.each(touches("working"), working, {
			key = function(n: any): string
				return n.id
			end,
		}),
		-- (A new flash of the same node is a new one: keyed by its change.)
		ui.each(touches("flashes"), flash, {
			key = function(n: any): string
				return `{n.version}:{n.id}`
			end,
		}),
	}
end

-- A Design tab with no Design yet, as the Browser's start page stands: New
-- design, Open design, and the Designs touched most recently on this
-- computer (agents' too, made with no tab open), read as it shows.
local function start(key: string, id: string): Node
	local function open(path: string)
		state.set_path(key, id, path)
	end
	local recent = ui.use_state(nil :: { any }?)
	ui.effect(function()
		api.call("design.recent", {}):after(function(r: any)
			recent:set(if type(r) == "table" and type(r.recent) == "table" then r.recent else {})
		end, function(err: any)
			said("recent designs")(err)
			recent:set({})
		end)
	end)
	local PanelHome = require("@ds/panel_home")
	local function row(r: any, i: number): Node
		return PanelHome.Row {
			id = `design-recent-{i}`,
			icon = "palette",
			label = r.name,
			tooltip = r.path,
			on_click = function()
				open(r.path)
			end,
		}
	end
	return ui.column {
		id = `design-start-{id}`,
		w_full = true,
		grow = 1,
		min_h = 0,
		items = "center",
		justify = "center",
		p = "$space.4",
		gap = "$space.4",
		ds.EmptyState {
			icon = "palette",
			size = "sm",
			title = ui.t("panel.design.empty.title") :: any,
			body = ui.t("panel.design.empty.body") :: any,
			action = {
				label = ui.t("panel.design.new") :: any,
				icon = "plus",
				on_click = function()
					api.call("design.new", {}):after(function(made: any)
						if type(made) == "table" and type(made.path) == "string" then
							open(made.path)
						end
					end, said("new design"))
				end,
			},
			secondary = {
				label = ui.t("panel.design.open") :: any,
				on_click = function()
					ui.open_files({
						title = ui.t("panel.design.open"):get(),
						filters = design_filter(),
					}):after(function(files: any)
						local first = if type(files) == "table" then files[1] else nil
						local picked = if type(first) == "table" then first.path else first
						if type(picked) == "string" and picked ~= "" then
							open(picked)
						end
					end, function() end)
				end,
			},
		},
		ui.column {
			id = `design-recent-{id}`,
			w_full = true,
			max_w = ui.theme.get("zeron.form.list") :: number?,
			gap = "$zeron.list.gap",
			PanelHome.Heading { label = ui.t("panel.design.recent") },
			ui.presence(function(): string
				local r = recent:get()
				return if r == nil then "loading" elseif #r == 0 then "none" else "some"
			end, function(which: string): Node
				if which == "none" then
					return ds.Text { ui.t("panel.design.no_recent"), style = "caption", tone = "tertiary" }
				elseif which == "loading" then
					return ui.column {}
				end
				return ui.each(ui.computed(function(): { any }
					return recent:get() or {}
				end), row, {
					key = function(r: any): string
						return r.path
					end,
				})
			end, { w_full = true }),
		},
	}
end

export type Props = { key: string, tab: string, [any]: any }

M.Design = ui.component(function(props: Props): Node
	local key, id = props.key, props.tab
	local host = shown(id)
	local function tab(): Tab?
		state.of(key)
		return state.find(key, id)
	end
	local function path(): string?
		local t = tab()
		return if t then t.path else nil
	end
	-- Its Design followed as it's named (and again after a reload: the host
	-- keeps one Canvas a tab).
	-- Its chat's Design tabs said as they change, and once more as it goes
	-- (closed, or another tab in front).
	ui.effect(function()
		M.say_tabs(key)
	end)
	ui.on_cleanup(function()
		ui.after(0, function()
			M.say_tabs(key)
		end)
	end)
	ui.effect(function()
		local p = path()
		if p then
			api.invoke("design.open", { tab = id, path = p })
		end
	end)
	-- Saved as another file: the tab's from then on.
	ui.effect(function()
		local b, t = host:get(), tab()
		if b and type(b.path) == "string" and b.path ~= "" and t and t.path and b.path ~= t.path then
			state.set_path(key, id, b.path)
		end
	end)
	local function save_as()
		local t = tab()
		if not t then
			return
		end
		ui.save_file({
			title = ui.t("panel.design.command.save_as"):get(),
			name = `{M.title(t) or ui.t("panel.tab.design"):get()}.zdesign`,
			filters = design_filter(),
		}):after(function(file: any)
			local to = if type(file) == "table" then file.path else file
			if type(to) == "string" and to ~= "" then
				api.call("design.save_as", { tab = id, to = to }):after(function() end, said("save as"))
			end
		end, function() end)
	end
	-- ⌘S and ⌘⇧S are this tab's while it shows.
	local t0 = tab()
	local mine = if t0 then { tab = t0, save_as = save_as } else nil
	if mine then
		front = mine
	end
	ui.effect(function()
		local t = tab()
		if t and mine then
			mine.tab = t
		end
	end)
	ui.on_cleanup(function()
		if front == mine then
			front = nil
		end
	end)
	-- Which shows: a Design's Canvas, or why not (a real swap).
	local function kind(): string
		if not path() then
			return "none"
		end
		local b = host:get()
		if b and b.state == "failed" then
			return "failed"
		end
		return "canvas"
	end
	-- Changes found in the backup as it opened: kept (dismissed here) or
	-- discarded, its file as it is then.
	local kept = ui.use_state(false)
	local recovered = ui.show(function(): boolean
		local b = host:get()
		return b ~= nil and b.recovered == true and b.unsaved == true and not kept:get()
	end, ui.row {
		id = `design-recovered-{id}`,
		w_full = true,
		items = "center",
		gap = "$space.2",
		px = "$space.3",
		py = "$space.2",
		bg = "$surface.raised",
		border = 1,
		border_sides = { 0, 0, 1, 0 },
		border_color = "$border.subtle",
		ds.Icon { name = "info-circle", size = 14 },
		ui.column {
			grow = 1,
			min_w = 0,
			ds.Text { ui.t("panel.design.recovered.title"), style = "label" },
			ds.Text {
				function(): string
					local b = host:get()
					local stale = b ~= nil and b.stale == true
					return ui.t(if stale then "panel.design.recovered.stale" else "panel.design.recovered.body"):get()
				end,
				style = "caption",
				tone = "secondary",
				wrap = true,
			},
		},
		ds.Button {
			ui.t("panel.design.recovered.keep"),
			tone = "secondary",
			size = "sm",
			on_click = function()
				kept:set(true)
			end,
		},
		ds.Button {
			ui.t("panel.design.recovered.discard"),
			tone = "ghost",
			size = "sm",
			on_click = function()
				local t = tab()
				if t then
					M.discard(t)
				end
			end,
		},
	})
	return ui.column {
		id = `panel-design-{id}`,
		w_full = true,
		grow = 1,
		min_h = 0,
		recovered,
		ui.presence(kind, function(which: string): Node
			if which == "none" then
				return start(key, id)
			elseif which == "failed" then
				return M.Empty {
					title = ui.t("panel.design.failed"),
					body = function(): string
						local b = host:get()
						return if b and b.error then b.error else ""
					end,
				}
			end
			return canvas(id)
		end, { w_full = true, grow = 1, min_h = 0 }),
	}
end)

return M
