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
local PanelHome = require("@ds/panel_home")
local ListRow = require("@ds/list_row").ListRow
local toast = require("@ds/toast")
local state = require("@app/screens/panel/state")
local api = require("@app/screens/panel/design_api")

type Tab = state.Tab

local M = {}

local ZOOM_MIN, ZOOM_MAX = 0.25, 5
-- How long after the last wheel step a zoom is over, and its tiles drawn
-- at its own level.
local SETTLE_MS = 150
-- How far a press may move and still be a click (selecting), not a drag.
local CLICK_SLOP = 4
-- How soon a second press on a selected text edits it.
local DOUBLE_MS = 400
-- Unseen: the in-place text field's words, caret and selection (the Canvas
-- draws them).
local CLEAR = "#00000000"
-- How long a property field waits after its last change to send it.
local PROPERTY_PAUSE_MS = 600
-- How long what an agent just changed flashes (pen.dev's live touches).
local FLASH_MS = 900

-- Each tab's view: where the Canvas's origin is in it, and its zoom.
type View = { x: number, y: number, zoom: number }
local views = ui.state("design_views", {} :: { [string]: View })
-- Recovered changes a tab was told to keep (`tab:path`): its notice put
-- away while the engine still holds them unsaved.
local kept = ui.state("design_kept", {} :: { [string]: boolean })
-- The Recent designs each start page read, and the visit it read them in
-- (a reload reads nothing again).
local recent_read = ui.state("design_recent_read", {} :: { [string]: { any } })
-- Each tab's selection (#32): the ids, in the order selected, kept across
-- reloads and told to the host (which tells the engine, for the chat's
-- agent).
local selections = ui.state("design_selections", {} :: { [string]: { string } })
-- The flashes already shown (`tab:version:node`), kept across reloads.
local flashed = ui.state("design_flashed", {} :: { [string]: boolean })

-- What the host says of a tab's Canvas (src/design.rs): its state, where
-- its Design draws and its tiles' layers.
local function shown(id: string): State<any>
	return ui.shared(`design.tab.{id}`, nil :: any)
end

-- Its layers list (the host's, set as the Design changes).
local function shown_tree(id: string): State<any>
	return ui.shared(`design.tree.{id}`, nil :: any)
end

-- The chat a panel's tabs are (`chat:<id>`), if they're a chat's.
local function chat_of(key: string): string?
	return string.match(key, "^chat:(.+)$")
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

-- What failed, said in the engine's words (the tab keeps its marker, so
-- nothing is lost by it).
local function said(title: string): (any) -> ()
	return function(err: any)
		toast.show {
			title = ui.t(title):get(),
			body = if type(err) == "table" and err.message then tostring(err.message) else tostring(err),
			tone = "danger",
		}
	end
end

-- One of the engine's calls on a tab's file (#28), then `done`.
local function on_file(call: string, tab: Tab, done: (() -> ())?)
	api.call(call, { tab = tab.id }):after(function()
		if done then
			done()
		end
	end, said("panel.design.failed_save"))
end

-- Its file written now (⌘S); `done` once it is (a close waits for it).
function M.save(tab: Tab, done: (() -> ())?)
	on_file("design.save", tab, done)
end

-- Back as its file holds it, its unsaved changes gone.
function M.discard(tab: Tab, done: (() -> ())?)
	on_file("design.discard", tab, done)
end

-- Whether closing it should ask Save or Discard: it has changes its file
-- doesn't, and isn't one of Zeron's own (those save themselves, a moment
-- after a change). Unknown, it asks.
function M.closing(tab: Tab, done: (ask: boolean) -> ())
	if not M.dirty(tab) then
		done(false)
		return
	end
	api.call("design.saving", { tab = tab.id }):after(function(saving: any)
		done(not (type(saving) == "table" and saving.managed == true))
	end, function()
		done(true)
	end)
end

-- Whether the panel shows (its shell's; set by screens/panel): ⌘S is a
-- Design tab's only then.
M.shown = function(): boolean
	return true
end

-- The Design tab in front of its panel now, and how to save it as another
-- file: ⌘S and ⌘⇧S act on it while the panel shows and it has a Design.
local front: { key: string, id: string, save_as: () -> () }? = nil
local function front_tab(): Tab?
	local f = front
	if not f or not M.shown() then
		return nil
	end
	local t = state.find(f.key, f.id)
	return if t and t.path then t else nil
end

local function design_filter(): { any }
	return { { ui.t("panel.design.filter"):get(), { "zdesign" } } }
end

ui.command {
	id = "panel.design.save",
	title = ui.t("panel.design.command.save"):get(),
	menu = "File",
	key = "cmd+s",
	run = function(): boolean
		local t = front_tab()
		if not t then
			return false
		end
		M.save(t)
		return true
	end,
}
ui.command {
	id = "panel.design.save_as",
	title = ui.t("panel.design.command.save_as"):get(),
	menu = "File",
	key = "cmd+shift+s",
	run = function(): boolean
		local f = front
		if not f or not front_tab() then
			return false
		end
		f.save_as()
		return true
	end,
}

-- The person's edits by hand on tab `id`'s Design (#33): one change, one
-- undo step, told to every tab showing it.
-- (`refused`, if given, hears it refused, after the person's told.)
local function edit(id: string, edits: { any }, description: string, join: boolean?, refused: (() -> ())?)
	local tell = said("panel.design.failed_edit")
	api.call("design.edit", { tab = id, edits = edits, description = description, join = join == true })
		:after(function() end, function(e: any)
			tell(e)
			if refused then
				refused()
			end
		end)
end

-- A step through the Design's one history, whoever's change is latest.
local function step(call: string)
	local t = front_tab()
	if not t then
		return false
	end
	api.call(call, { tab = t.id }):after(function() end, said("panel.design.failed_edit"))
	return true
end
ui.command {
	id = "panel.design.undo",
	title = ui.t("panel.design.command.undo"):get(),
	menu = "Edit",
	key = "cmd+z",
	run = function(): boolean
		return step("design.undo")
	end,
}
ui.command {
	id = "panel.design.redo",
	title = ui.t("panel.design.command.redo"):get(),
	menu = "Edit",
	key = "cmd+shift+z",
	run = function(): boolean
		return step("design.redo")
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
	local chat = chat_of(key)
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

-- Sets what tab `id` has selected: `ids` (none: nothing).
local function select(id: string, ids: { string })
	selections:set_key(id, ids)
end

-- A click on `node` in tab `id` (nothing: off any node): it alone
-- selected, or with `add` (Shift) added, or taken away if it was.
local function pick(id: string, node: string?, add: boolean)
	local now = selections:peek()[id] or {}
	if not add then
		select(id, if node then { node } else {})
		return
	end
	if not node then
		return
	end
	local picked = table.clone(now)
	local at = table.find(picked, node)
	if at then
		table.remove(picked, at)
	else
		table.insert(picked, node)
	end
	select(id, picked)
end

-- The layers list: the Design's tree, a row a node, indented by its
-- depth; a click selects (Shift adds), as on the Canvas.
local function layers(id: string): Node
	local tree = shown_tree(id)
	local function row(n: any): Node
		return ListRow {
			id = `design-layer-{id}-{n.id}`,
			class = "design-layer-row",
			label = n.name,
			h = ui.raw(26, "a layer row, as dense as a file tree's"),
			selected = function(): boolean
				return table.find(selections:get()[id] or {}, n.id) ~= nil
			end,
			on_click = function()
				pick(id, n.id, ui.modifiers.shift:peek())
			end,
			ui.row {
				w_full = true,
				h_full = true,
				items = "center",
				gap = "$space.1",
				px = "$space.2",
				pl = ui.raw(8 + 12 * n.depth, "a layer's depth, a step a level"),
				ds.Icon { name = if n.type == "text" then "document" elseif n.type == "frame" or n.type == "instance" then "widget" else "pen", size = 12 },
				ds.Text { n.name, style = "caption", truncate = true },
			},
		}
	end
	return ui.column {
		id = `design-layers-{id}`,
		role = "tree",
		label = ui.t("panel.design.layers"),
		w = ui.raw(220, "the layers list's width"),
		h_full = true,
		shrink = 0,
		border = 1,
		border_sides = { 0, 1, 0, 0 },
		border_color = "$border.subtle",
		ui.scroll {
			w_full = true,
			grow = 1,
			min_h = 0,
			py = "$space.1",
			ui.each(ui.computed(function(): { any }
				local t = tree:get()
				return if type(t) == "table" then t else {}
			end), row, {
				key = function(n: any): string
					return n.id
				end,
			}),
		},
	}
end

-- The properties panel (#33): the one node selected, its common
-- properties, each sent as an edit when committed (Enter, or leaving the
-- field). Numbers stay numbers; a `$name` refers to a Token.
local FIELDS: { { key: string, label: string, number: boolean } } = {
	{ key = "fill", label = "panel.design.prop.fill", number = false },
	{ key = "width", label = "panel.design.prop.width", number = true },
	{ key = "height", label = "panel.design.prop.height", number = true },
	{ key = "gap", label = "panel.design.prop.gap", number = true },
	{ key = "padding", label = "panel.design.prop.padding", number = true },
	{ key = "cornerRadius", label = "panel.design.prop.radius", number = true },
	{ key = "fontSize", label = "panel.design.prop.font_size", number = true },
	{ key = "fontFamily", label = "panel.design.prop.font", number = false },
}
local function properties(id: string): Node
	local host = shown(id)
	local function props(): any
		local b = host:get()
		return if b then b.props else nil
	end
	-- A property as its field shows it: blank for one it can't (none, or
	-- a list such as a padding per side).
	local function shown(v: any): string
		return if v == nil or type(v) == "table" then "" else tostring(v)
	end
	local function field(f: { key: string, label: string, number: boolean }, node: string, start: any): Node
		local value = ui.use_state(shown(start))
		-- What the Design last said, as the field shows it.
		local known = shown(start)
		local typing = false
		local sent: string? = nil
		local pending: (() -> ())? = nil
		local function commit()
			if pending then
				pending()
				pending = nil
			end
			-- (Committed as shown, or as just sent (Enter, then leaving), it
			-- changes nothing: a blank list stays.)
			local text = value:peek()
			if text == known or text == sent then
				return
			end
			sent = text
			local v: any = text
			if f.number and tonumber(text) then
				v = tonumber(text)
			end
			-- (Refused, it shows what the Design says again.)
			edit(id, { { update = node, props = { [f.key] = v } } }, "Change a property", false, function()
				sent = nil
				value:set(known)
			end)
		end
		-- (Following what the Design says as it changes, by anyone, but not
		-- over what's being typed.)
		ui.effect(function()
			local now = props()
			if now and now.id == node then
				known = shown(now[f.key])
				if pending == nil then
					value:set(known)
				end
			end
		end)
		-- (Typed, it's kept on Enter or on leaving the field. Set from
		-- outside, by assistive technology with the field unfocused, it's
		-- kept a moment after it stops changing, as the settings' fields
		-- are; and either, as the selection moves on.)
		ui.effect(function()
			local text = value:get()
			-- (Running again cancels the wait it made last.)
			pending = nil
			if text == known or typing then
				return
			end
			pending = ui.after(PROPERTY_PAUSE_MS, function()
				pending = nil
				commit()
			end)
		end)
		ui.on_cleanup(function()
			if pending or typing then
				commit()
			end
		end)
		return ds.TextField {
			label = ui.t(f.label),
			value = value,
			size = "sm",
			w_full = true,
			on_commit = function()
				commit()
			end,
			on_focus_change = function(focused: boolean)
				typing = focused
			end,
			-- (Escape takes back what's typed: the Design's value again.)
			on_key = function(e: any): boolean?
				if e.key == "Escape" and value:peek() ~= known then
					value:set(known)
					return true
				end
				return nil
			end,
		}
	end
	-- (Always there, so the Canvas keeps its width as the selection changes.)
	local function panel(rows: { Node }): Node
		return ui.scroll {
			id = `design-props-{id}`,
			role = "group",
			label = ui.t("panel.design.properties"),
			w = ui.raw(220, "the properties panel's width, as the layers list's"),
			h_full = true,
			shrink = 0,
			border = 1,
			border_sides = { 0, 0, 0, 1 },
			border_color = "$border.subtle",
			p = "$space.3",
			gap = "$space.2",
			table.unpack(rows),
		}
	end
	return ui.presence(function(): string
		local p = props()
		return if type(p) == "table" and p.id then p.id else ""
	end, function(node: string): Node
		if node == "" then
			return panel({ ds.Text { ui.t("panel.design.select_one"), style = "caption", tone = "tertiary", wrap = true } })
		end
		local p = props()
		local rows: { Node } = { ds.Text { p.name or p.type or "", style = "label" } }
		for _, f in FIELDS do
			-- (What its type has, and what's set.)
			local has = p[f.key] ~= nil
				or (f.key == "fill")
				or ((f.key == "width" or f.key == "height") and p.type ~= "text")
				or ((f.key == "gap" or f.key == "padding") and p.type == "frame")
				or ((f.key == "fontSize" or f.key == "fontFamily") and p.type == "text")
			if has then
				table.insert(rows, field(f, node, p[f.key]))
			end
		end
		return panel(rows)
	end, { h_full = true })
end

-- Over the Canvas: the Theme it's shown in (an axis each, the Design's own
-- Themes; the tab's alone, agents' screenshots keep the first values), and
-- undo and redo.
local function toolbar(id: string): Node
	local host = shown(id)
	local function axes(): { { axis: string, values: { string } } }
		local b = host:get()
		local out = {}
		if b and type(b.themes) == "table" then
			for axis, values in b.themes do
				if type(values) == "table" then
					table.insert(out, { axis = axis, values = values })
				end
			end
		end
		table.sort(out, function(a, c)
			return a.axis < c.axis
		end)
		return out
	end
	local function choose(axis: string, value: string)
		local b = host:peek()
		local theme = table.clone(if b and type(b.theme) == "table" then b.theme else {})
		theme[axis] = value
		api.invoke("design.theme", { tab = id, theme = theme })
	end
	local function axis_row(a: any): Node
		local function shown_in(): string
			local b = host:peek()
			local chosen = if b and type(b.theme) == "table" then b.theme[a.axis] else nil
			return chosen or a.values[1]
		end
		local chosen = ui.use_state(shown_in())
		ui.effect(function()
			local value = chosen:get()
			if value ~= shown_in() then
				choose(a.axis, value)
			end
		end)
		local options = {}
		for _, value in a.values do
			table.insert(options, { value = value, label = value })
		end
		return ds.Segmented {
			id = `design-theme-{id}-{a.axis}`,
			label = a.axis,
			value = chosen,
			options = options,
		}
	end
	return ui.row {
		id = `design-toolbar-{id}`,
		w_full = true,
		items = "center",
		gap = "$space.2",
		p = "$space.1",
		border = 1,
		border_sides = { 0, 0, 1, 0 },
		border_color = "$border.subtle",
		ui.each(ui.computed(axes), axis_row, {
			key = function(a: any): string
				return a.axis
			end,
		}),
		ui.div { grow = 1 },
		ds.IconButton {
			label = ui.t("panel.design.command.undo"),
			size = "sm",
			tone = "quiet",
			on_click = function()
				step("design.undo")
			end,
			ds.Icon { name = "arrow-left", size = 14 },
		},
		ds.IconButton {
			label = ui.t("panel.design.command.redo"),
			size = "sm",
			tone = "quiet",
			on_click = function()
				step("design.redo")
			end,
			ds.Icon { name = "arrow-right", size = 14 },
		},
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
	local pointer: { x: number, y: number, from_x: number, from_y: number, moving: { string }? }? = nil
	-- Selected nodes being dragged, and how far (Canvas units): their
	-- outlines follow; the move is sent as the drag ends (#33).
	local dragging = ui.use_state(nil :: { ids: { string }, dx: number, dy: number }?)
	-- A text being edited in place: its node, the words as typed, the caret
	-- (and the other end of what's selected) as char places, and whether
	-- it's sent a change yet. The words are held by a real text field, out
	-- of sight over the text (`field`): what's typed, an input method's,
	-- pasted or set by assistive technology goes in as it does in any.
	local editing = ui.use_state(nil :: { node: string, content: string, at: number, anchor: number, typed: boolean }?)
	local last_press: { node: string?, at: number }? = nil
	local field = ui.ref()
	-- (Whether the field has the focus; and the Canvas, which takes it as an
	-- edit ends there, rather than the next field along.)
	local field_focused = false
	local surface = ui.ref()
	local function canvas_point(e: any): (number, number)
		local v = views:peek()[id] :: View
		return (e.local_x - v.x) / v.zoom, (e.local_y - v.y) / v.zoom
	end
	-- What tab `id` has selected that's a text, alone: its words.
	local function single_text(node: string): string?
		local b = host:peek()
		local props = if b then b.props else nil
		if type(props) == "table" and props.id == node and props.type == "text" then
			return props.content or ""
		end
		return nil
	end
	local function start_editing(node: string)
		local content = single_text(node)
		if not content then
			return
		end
		local count = utf8.len(content) or #content
		editing:set({ node = node, content = content, at = count, anchor = count, typed = false })
	end
	-- The edit over: the focus to the Canvas, if the field has it (the
	-- field leaving with it would hand it to the next field along), or
	-- always, for a press on the Canvas.
	local function finish_editing(pressed: boolean?)
		local canvas_el = surface:peek()
		if (field_focused or pressed) and canvas_el then
			canvas_el:focus()
		end
		editing:set(nil)
	end
	-- The field's caret and selection: anchor and caret, in chars.
	local function selection(): (number?, number?)
		local el = field:peek()
		if not el then
			return nil, nil
		end
		return el:selection()
	end
	-- The field's caret and selection, as the edit's (the caret drawn there).
	local function follow_caret()
		local e = editing:peek()
		local anchor, at = selection()
		if e and anchor and at and (at ~= e.at or anchor ~= e.anchor) then
			editing:set({ node = e.node, content = e.content, at = at, anchor = anchor, typed = e.typed })
		end
	end
	-- The field's words changed: sent as they change.
	local function typed(content: string)
		local e = editing:peek()
		if not e or content == e.content then
			return
		end
		-- (The words kept before the field's caret is read: reading it
		-- settles the field, which would otherwise show the words before.)
		editing:set({ node = e.node, content = content, at = e.at, anchor = e.anchor, typed = true })
		follow_caret()
		-- (After its first, an edit's changes join it: one undo step.)
		edit(id, { { update = e.node, props = { content = content } } }, "Edit text", e.typed)
	end
	-- The keys the field gives way to: Escape and Enter end the edit (Shift
	-- and Enter breaks the line), and ⌘Z, ⌘⇧Z and Ctrl+Y step the Design's history,
	-- not the field's (whose words would write over what they take back).
	local function on_field_key(k: any): boolean?
		if k.key == "Escape" or (k.key == "Enter" and not k.shift) then
			finish_editing()
			return true
		end
		local z, y = k.key == "z" or k.key == "Z", k.key == "y" or k.key == "Y"
		if (z or y) and (k.cmd or k.ctrl or k.meta) then
			finish_editing()
			step(if y or k.shift then "design.redo" else "design.undo")
			return true
		end
		return nil
	end
	-- Where the caret and what's selected are, as the text is drawn (laid
	-- out once a change, for all the overlays reading it).
	local carets = ui.computed(function(): any
		local e = editing:get()
		if not e then
			return nil
		end
		return api.call_sync("design.caret", { tab = id, id = e.node, content = e.content, at = e.at, from = e.anchor, to = e.at })
	end)

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
	-- What the person selected: outlined.
	local function selected(n: any): Node
		-- (Where a drag is taking it.)
		local function by(): (number, number)
			local d = dragging:get()
			if d and table.find(d.ids, n.id) then
				return d.dx, d.dy
			end
			return 0, 0
		end
		local box = over({
			id = n.id,
			x = n.x,
			y = n.y,
			w = n.w,
			h = n.h,
		})
		local at_left, at_top = box.left, box.top
		box.left = function(): number
			local v, dx = view(), by()
			return at_left() + (if v then dx * v.zoom else 0)
		end
		box.top = function(): number
			local v = view()
			local _, dy = by()
			return at_top() + (if v then dy * v.zoom else 0)
		end
		box.id = `design-selected-{id}-{n.id}`
		box.class = "design-selected"
		box.aria_hidden = true
		box.border = 2
		box.border_color = "$accent"
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
		-- (Focused by its own presses, not as a press's default: while text
		-- is edited, a press on it, a double one too, leaves the field the
		-- focus.)
		ref = surface,
		focusable = true,
		focus_on_press = false,
		focus = { outline = 0 },
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
		-- A drag pans; a press that doesn't move selects what's under it:
		-- Shift adds (or takes away), ⌘/Ctrl picks the innermost node.
		-- A press on a selected node and a drag moves the selection (#33);
		-- a second press on a selected text, at once, edits it in place.
		on_press = function(e: any)
			if not views:peek()[id] then
				return
			end
			local x, y = canvas_point(e)
			local under = api.call_sync("design.hit", { tab = id, x = x, y = y })
			local chosen = selections:peek()[id] or {}
			local now = ui.now()
			local again = last_press and last_press.node == under and now - last_press.at < DOUBLE_MS
			last_press = { node = under, at = now }
			local e_now = editing:peek()
			if e_now then
				-- (Editing: a press on its text places the caret; elsewhere
				-- it ends.)
				if under == e_now.node then
					local place = api.call_sync("design.caret", { tab = id, id = e_now.node, content = e_now.content, x = x, y = y })
					local el = field:peek()
					if type(place) == "number" and el then
						el:set_selection(place)
						follow_caret()
						-- (A press on the Canvas leaves focus where it is:
						-- the field keeps it, given back if it was lost.)
						el:focus()
					end
					return
				end
				finish_editing(true)
			else
				-- (Not editing: the Canvas takes the focus, from a property
				-- field, say, which keeps what's typed in it as it goes.)
				local canvas_el = surface:peek()
				if canvas_el then
					canvas_el:focus()
				end
			end
			if again and type(under) == "string" and table.find(chosen, under) and single_text(under) then
				start_editing(under)
				return
			end
			-- (On a selected node, however deep: a nested one selected with
			-- ⌘/Ctrl is under the pointer too.)
			local innermost = api.call_sync("design.hit", { tab = id, x = x, y = y, deep = true })
			local on_selected = (type(under) == "string" and table.find(chosen, under) ~= nil)
				or (type(innermost) == "string" and table.find(chosen, innermost) ~= nil)
			local moving = if on_selected then table.clone(chosen) else nil
			pointer = { x = e.local_x, y = e.local_y, from_x = e.local_x, from_y = e.local_y, moving = moving }
		end,
		on_pointer_move = function(e: any)
			local p = pointer
			if not p then
				return
			end
			if p.moving then
				local v = views:peek()[id] :: View
				dragging:set({ ids = p.moving, dx = (e.local_x - p.from_x) / v.zoom, dy = (e.local_y - p.from_y) / v.zoom })
			else
				pan(e.local_x - p.x, e.local_y - p.y)
			end
			p.x, p.y = e.local_x, e.local_y
		end,
		on_release = function(e: any)
			local p = pointer
			pointer = nil
			local v = views:peek()[id]
			local far = p and math.abs(e.local_x - p.from_x) + math.abs(e.local_y - p.from_y) > CLICK_SLOP
			if p and p.moving and far and v then
				-- The moves, one change: each node's new place, as the
				-- host works it out (an auto layout's order, or x and y).
				local dx, dy = (e.local_x - p.from_x) / v.zoom, (e.local_y - p.from_y) / v.zoom
				local edits: { any } = {}
				for _, node in p.moving do
					local moved = api.call_sync("design.moved", { tab = id, id = node, dx = dx, dy = dy })
					if moved then
						table.insert(edits, moved)
					end
				end
				dragging:set(nil)
				if #edits > 0 then
					edit(id, edits, "Move")
				end
				return
			end
			dragging:set(nil)
			if not p or not v or far then
				return
			end
			local held = ui.modifiers
			local deep = held.control:peek() or held.meta:peek()
			local node = api.call_sync("design.hit", {
				tab = id,
				x = (e.local_x - v.x) / v.zoom,
				y = (e.local_y - v.y) / v.zoom,
				deep = deep,
			})
			pick(id, if type(node) == "string" then node else nil, held.shift:peek())
		end,
		ui.each(ui.computed(function(): { any }
			local b = told()
			return if b then b.layers else {}
		end), layer, {
			key = function(l: any): string
				return tostring(l.level)
			end,
		}),
		ui.each(touches("selected"), selected, {
			key = function(n: any): string
				return n.id
			end,
		}),
		-- Editing text: what's selected of it, and the caret, where the
		-- drawn glyphs are.
		ui.each(ui.computed(function(): { any }
			local c = carets:get()
			return if c and c.range then c.range else {}
		end), function(r: any, i: number): Node
			local o = over({ x = r.x, y = r.y, w = r.w, h = r.h })
			o.class = "design-text-range"
			o.aria_hidden = true
			o.bg = "$selection.item"
			return ui.div(o)
		end),
		ui.show(function(): boolean
			return editing:get() ~= nil
		end, ui.component(function(_: {}): Node
			-- Over the text, where an input method puts its window; its own
			-- words, caret and selection unseen (the Canvas draws them), and
			-- clicks going on to the Canvas.
			-- (Where the text's box is as it's typed: a text as wide as its
			-- words grows.)
			local o = over({
				x = 0,
				y = 0,
				w = 0,
				h = 0,
			})
			local function edge(key: string, view_at: (v: View) -> number, scale: boolean): () -> number
				return function(): number
					local c, v = carets:get(), view()
					if not c or not v then
						return 0
					end
					return (if scale then 0 else view_at(v)) + c.box[key] * v.zoom
				end
			end
			local left = edge("x", function(v: View): number
				return v.x
			end, false)
			o.top = edge("y", function(v: View): number
				return v.y
			end, false)
			-- (Held inside the Canvas as the words go past either side of it:
			-- the field scrolls its own text, nothing around it.)
			local wide = edge("w", function(): number
				return 0
			end, true)
			o.left = function(): number
				return math.clamp(left(), 0, math.max(size:get().w - 1, 0))
			end
			o.w = function(): number
				return math.max(math.min(left() + wide(), size:get().w) - o.left(), 1)
			end
			o.h = edge("h", function(): number
				return 0
			end, true)
			o.id = `design-text-field-{id}`
			o.label = ui.t("panel.design.edit_text"):get()
			o.ref = field
			o.value = function(): string
				local now = editing:get()
				return if now then now.content else ""
			end
			o.on_change = typed
			o.on_caret = function()
				follow_caret()
			end
			o.on_key = on_field_key
			-- (Left for elsewhere in the window, the edit ends: its words
			-- aren't kept over what changes while it's away. The window left
			-- for another app keeps it.)
			o.on_focus = function()
				field_focused = true
			end
			o.on_blur = function()
				field_focused = false
				ui.after(0, function()
					if not field_focused and editing:peek() and ui.window.active:peek() then
						editing:set(nil)
					end
				end)
			end
			-- (Focused at once; its caret put where the edit starts as its
			-- words have come in, a frame on, unless it's been typed in.)
			o.on_mount = function(el: any)
				if editing:peek() then
					el:focus()
				end
				ui.after(0, function()
					local now = editing:peek()
					if now and not now.typed and field.mounted then
						el:set_selection(now.anchor, now.at)
					end
				end)
			end
			-- (Its lines broken only where the words break them: wrapped at its
			-- own width, held inside the Canvas, it would break them elsewhere.)
			o.wrap = false
			o.pointer_events = "none"
			o.color = CLEAR
			o.caret_color = CLEAR
			o.selection_color = CLEAR
			o.bg = CLEAR
			o.border = 0
			o.focus = { outline = 0 }
			o.px = 0
			o.py = 0
			return ui.textarea(o)
		end)({})),
		ui.show(function(): boolean
			return editing:get() ~= nil
		end, ui.div {
			id = `design-caret-{id}`,
			class = "design-caret",
			aria_hidden = true,
			absolute = true,
			w = 2,
			bg = "$accent",
			left = function(): number
				local c, v = carets:get(), view()
				return if c and v then v.x + c.caret.x * v.zoom - 1 else 0
			end,
			top = function(): number
				local c, v = carets:get(), view()
				return if c and v then v.y + c.caret.y * v.zoom else 0
			end,
			h = function(): number
				local c, v = carets:get(), view()
				return if c and v then c.caret.h * v.zoom else 0
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
	-- Read as the start page shows, once a showing (kept across a reload).
	local function recent(): { any }?
		return recent_read:get()[id]
	end
	if recent_read:peek()[id] == nil then
		ui.effect(function()
			api.call("design.recent", {}):after(function(r: any)
				recent_read:set_key(id, if type(r) == "table" and type(r.recent) == "table" then r.recent else {})
			end, function()
				recent_read:set_key(id, {})
			end)
		end)
	end
	-- (Read again the next time it shows.)
	ui.on_cleanup(function()
		recent_read:set_key(id, nil)
	end)
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
					end, said("panel.design.failed_new"))
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
				local r = recent()
				return if r == nil then "loading" elseif #r == 0 then "none" else "some"
			end, function(which: string): Node
				if which == "none" then
					return ds.Text { ui.t("panel.design.no_recent"), style = "caption", tone = "tertiary" }
				elseif which == "loading" then
					return ui.column {}
				end
				return ui.each(ui.computed(function(): { any }
					return recent() or {}
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
	-- What's no longer in the Design (deleted, or another Design opened in
	-- the tab) leaves the selection. (A node inside an Instance isn't a row
	-- of the list: kept while its Instance is.)
	local tree = shown_tree(id)
	ui.effect(function()
		local t = tree:get()
		local ids = selections:peek()[id]
		if type(t) ~= "table" or not ids or #ids == 0 then
			return
		end
		local here: { [string]: boolean } = {}
		for _, n in t do
			here[n.id] = true
		end
		local kept: { string } = {}
		for _, node in ids do
			if here[string.match(node, "^([^/]+)") or node] then
				table.insert(kept, node)
			end
		end
		if #kept ~= #ids then
			select(id, kept)
		end
	end)
	-- Its selection told to the host as it changes (the host passes it on
	-- only when it changed), for the chat's agent.
	ui.effect(function()
		local ids = selections:get()[id] or {}
		if path() then
			api.invoke("design.select", { tab = id, chat = chat_of(key) or "", ids = ids })
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
				api.call("design.save_as", { tab = id, to = to }):after(function() end, said("panel.design.failed_save"))
			end
		end, function() end)
	end
	-- ⌘S and ⌘⇧S are this tab's while it's its panel's front tab.
	local mine = { key = key, id = id, save_as = save_as }
	ui.effect(function()
		if state.of(key).active == id then
			front = mine
		elseif front == mine then
			front = nil
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
	-- Changes found in the backup as it opened: kept (the notice put away
	-- for this file) or discarded, its file as it is then.
	local function kept_key(): string
		local b = host:peek()
		return `{id}:{if b and b.path then b.path else ""}`
	end
	local recovered = ui.show(function(): boolean
		local b = host:get()
		return b ~= nil and b.recovered == true and b.unsaved == true and not kept:get()[`{id}:{b.path or ""}`]
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
				kept:set_key(kept_key(), true)
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
			return ui.row {
				w_full = true,
				grow = 1,
				min_h = 0,
				layers(id),
				ui.column {
					grow = 1,
					min_w = 0,
					h_full = true,
					toolbar(id),
					canvas(id),
				},
				properties(id),
			}
		end, { w_full = true, grow = 1, min_h = 0 }),
	}
end)

return M
