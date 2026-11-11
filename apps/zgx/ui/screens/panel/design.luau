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
-- yet offers to open one.
--
--   Design.Design { key, tab }
--   Design.title(tab)     the Design's name
--   Design.close(tab)     its tiles let go
--   Design.fit(id)        the tab's view on all of its Design again
--   Design.Empty { title, body, action? }   what shows in place of a Canvas
--   Design.say_tabs(key)  the engine told which Designs the chat's tabs show

local ds = require("@ds")
local motion = require("@ds/motion")
local state = require("@app/screens/panel/state")

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

-- What the host says of a tab's Canvas (src/design.rs): its state, where
-- its Design draws and its tiles' layers.
local function shown(id: string): State<any>
	return ui.shared(`design.tab.{id}`, nil :: any)
end

function M.title(tab: Tab): string?
	local path = tab.path
	if not path then
		return nil
	end
	return string.match(path, "([^/\\]+)%.zdesign$") or string.match(path, "([^/\\]+)$")
end

function M.close(tab: Tab)
	ui.invoke("design.close", tab.id)
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
	ui.invoke("design.tabs", { chat = chat, paths = paths })
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
		ui.invoke("design.view", { tab = id, x = v.x, y = v.y, zoom = v.zoom, w = s.w, h = s.h, settled = settled:get() })
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
		local lit = ui.use_state(true)
		ui.after(0, function()
			lit:set(false)
		end)
		ui.after(FLASH_MS, function()
			ui.invoke("design.flashed", { tab = id, version = n.version })
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
			px = "$space.2",
			py = "$zeron.space.hair",
			rounded = "$radius.full",
			bg = "$accent",
			-- (The label style, in the ink that reads on the accent.)
			ui.text {
				ui.t("panel.design.in_progress"),
				font = "$font.sans",
				text_size = "$font.size.xs",
				line_height = "$font.line.xs",
				weight = "$font.weight.medium",
				color = "$accent.text",
			},
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
			ui.invoke("design.open", { tab = id, path = p })
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
	return ui.column {
		id = `panel-design-{id}`,
		w_full = true,
		grow = 1,
		min_h = 0,
		ui.presence(kind, function(which: string): Node
			if which == "none" then
				return M.Empty {
					title = ui.t("panel.design.empty.title"),
					body = ui.t("panel.design.empty.body"),
					action = {
						label = ui.t("panel.design.open") :: any,
						on_click = function()
							ui.open_files({
								title = ui.t("panel.design.open"):get(),
								filters = { { ui.t("panel.design.filter"):get(), { "zdesign" } } },
							}):after(function(files: any)
								local first = if type(files) == "table" then files[1] else nil
								if type(first) == "string" then
									state.set_path(key, id, first)
								end
							end, function() end)
						end,
					},
				}
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
