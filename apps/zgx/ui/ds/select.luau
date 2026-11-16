-- A select: a field-shaped button showing the chosen option, which opens
-- the options as a menu. A few short options open as a native pop-up
-- button's do: the chosen one laid exactly over the button, its glyph, name
-- and check on the button's glyph, name and chevron, the panel overhanging
-- the button by its own inset either side, so nothing moves but the rows
-- that appear round it. Options with a line under them, or too many to hold
-- still, drop below it instead, its width, scrolling past a dozen rows.
-- For a long list to search (projects, branches) use Dropdown.
--
-- `Select { value = state, options = { { value = "system", label = "Match system" }, … } }`.
-- Options may carry an icon and a line saying what they do, and a `group`:
-- the options of one group follow each other under its heading ("Built in",
-- "Imported"), and a grouped menu drops below the button. The label, hint
-- and error sit round it as round a text field.

local motion = require("@self/motion")
local Icon = require("@self/icon")
local FadeText = require("@self/fade_text")
local Menu = require("@self/menu")
local control = require("@self/control")
local material = require("@self/material")
local field = require("@self/field")
local locale = require("@self/locale")

-- (An option's label, line and group may be messages, `ui.t`, in a list
-- given as a table; a function's list reads them as strings, as a state
-- holds no message. The placeholder, label, hint and name take messages.)
-- `rule`: a rule over it in the menu, setting it apart from those before
-- (a menu with one drops below the button).
export type Option = { value: any, label: string | Message, icon: Icon.Name?, description: (string | Message)?, disabled: boolean?, group: (string | Message)?, rule: boolean? }
export type Props = {
	value: State<any>,
	-- The choices: a list, or a function of one, followed as it changes
	-- (devices coming and going): nothing is built again for it. (A state
	-- of one works too, read as `function() return s:get() end`: typed
	-- as a table, it would leave a literal list two tables to match.)
	options: { Option } | () -> { Option },
	-- (These too may be a state or a function, and are followed.)
	placeholder: (control.Live<string> | Message)?,
	label: (control.Live<string> | Message)?,
	hint: (control.Live<string> | Message)?,
	error: Props_error,
	size: control.Size?,
	disabled: Props_disabled,
	-- Its accessible name when it has no label.
	name: Label?,
	on_change: ((value: any) -> ())?,
	w: number?,
	w_full: boolean?,
	-- The menu always drops below the button, never over it.
	below: boolean?,
	-- The button only as wide as the chosen name and its chevron, no glyph
	-- (a row's compact control); its menu drops below, at its end.
	fit: boolean?,
}

-- How many rows open over the button; more drop below it. And the height
-- that list scrolls past.
local OVER_MOST = 8
local VIEW = 312
-- What a panel adds outside its rows, either side (its 1 edge, its 4 inset):
-- rows that line up with the button need a panel this much wider each way.
local OVERHANG = 5
-- Between rows (`zeron.list.gap`, in points for the list's sums).
local GAP = 2
-- A fitted button's menu, at least this wide (its names, not the button's).
local FIT_MENU = 200

return ui.component(function(props: Props): Node
	local size: control.Size = props.size or "md"
	local fit = props.fit == true
	local value = props.value
	local open = ui.use_state(false)
	local hovered = ui.use_state(false)
	-- The row the keys or the pointer are on (its place in the options).
	local lit = ui.use_state(0)
	local box = ui.ref()
	local offset = ui.use_state(0)
	-- The options as they stand (read in a binding, followed).
	local given: { Option } | () -> { Option } = props.options
	local function options(): { Option }
		return control.value(given) :: { Option }
	end
	local function chosen(): Option?
		local v = value:get()
		for _, o in options() do
			if o.value == v then
				return o
			end
		end
		return nil
	end
	-- Whether any option has an icon, or a line under its name; and whether
	-- the menu opens over the button (short and one line each).
	-- (`short`: a few rows dropped below, as tall as they are, no scroll.)
	local shape = ui.computed(function(): { icons: boolean, tall: boolean, over: boolean, short: boolean }
		local list = options()
		local icons, tall, grouped, ruled = false, false, false, false
		for _, o in list do
			icons = icons or o.icon ~= nil
			tall = tall or o.description ~= nil
			grouped = grouped or o.group ~= nil
			ruled = ruled or o.rule == true
		end
		local below = props.below == true or fit or ruled
		local few = not tall and not grouped and #list <= OVER_MOST
		return { icons = icons, tall = tall, over = few and not below, short = few and below }
	end)
	local function invalid(): boolean
		local e = control.read(props.error)
		return if e and e ~= "" then true else false
	end

	local trigger = ui.button {
		ref = box,
		label = props.label or props.name,
		disabled = props.disabled,
		h = control.H[size],
		px = control.PAD[size],
		pr = "$space.2",
		gap = "$space.2",
		w_full = not fit,
		rounded = control.RADIUS[size],
		border = 1,
		cursor = function(): any
			return if control.value(props.disabled) then "default" else "pointer"
		end,
		expanded = function()
			return open:get()
		end,
		bg = function()
			return if material.see_through() then (if hovered:get() or open:get() then "$zeron.field.glass_hover" else "$zeron.field.glass") elseif hovered:get() or open:get() then "$zeron.field.hover" else "$zeron.field.bg"
		end,
		border_color = function()
			if invalid() then
				return "$zeron.field.edge_error"
			elseif open:get() then
				return "$zeron.field.edge_focus"
			elseif hovered:get() then
				return "$zeron.field.edge_hover"
			end
			return "$zeron.field.edge"
		end,
		hover = {},
		active = {},
		-- The ring, its edge kept (a frame's edge never leaves).
		focus = {
			outline = "$zeron.focus.width",
			outline_offset = "$zeron.focus.offset",
			outline_color = "$zeron.focus.ring",
		},
		on_hover = function(inside: boolean)
			hovered:set(inside)
		end,
		on_click = function()
			open:toggle()
		end,
		-- The arrows open it too, as a native pop-up button's do.
		on_key = function(e)
			if not open:peek() and (e.key == "ArrowDown" or e.key == "ArrowUp") then
				open:set(true)
				return true
			end
			return false
		end,
		ui.show(function(): boolean
				local o = chosen()
				return not fit and o ~= nil and o.icon ~= nil
			end, ui.div {
				shrink = 0,
				y = -0.5,
				color = "$text.secondary",
				Icon {
					name = function(): Icon.Name
						local o = chosen()
						return (o and o.icon) or "widget"
					end,
					size = control.ICON[size],
				},
			}),
		(if fit then ui.text else FadeText) {
			function()
				local o = chosen()
				return if o then (control.text(o.label) or "") else (control.text(props.placeholder) or locale.s("ds.select.placeholder"))
			end,
			grow = if fit then nil else 1,
			text_size = control.TEXT[size],
			color = function()
				return if chosen() then "$text.primary" else "$zeron.field.placeholder"
			end,
		},
		ui.div {
			shrink = 0,
			color = function()
				return if open:get() then "$text.secondary" else "$text.tertiary"
			end,
			rotate = motion.spring(open, { from = 0, to = 180, stiffness = 380, damping = 30 }),
			Icon { name = "alt-arrow-down", size = 14 },
		},
	}

	-- A glyph's box at the button's icon size, so the name after it starts
	-- where the button's does.
	local function glyph(name: Icon.Name?, ink: string?): Node
		local side = control.ICON[size]
		return ui.div {
			w = side,
			h = side,
			shrink = 0,
			y = -0.5,
			items = "center",
			justify = "center",
			color = ink or "$text.secondary",
			if name then Icon { name = name, size = side } else nil,
		}
	end

	local function row(i: number, o: Option, rows: { any }, has_icons: boolean, tall: boolean): Node
		local enabled = not o.disabled
		local ink = if enabled then "$text.primary" else "$text.disabled"
		local function on(): boolean
			return value:get() == o.value
		end
		local name = FadeText { o.label, grow = 1, text_size = control.TEXT[size], color = ink }
		local p = Menu.lights(Menu.look({
			id = `select-option-{i}`,
			label = o.label,
			disabled = o.disabled,
			on_click = function()
				value:set(o.value)
				if props.on_change then
					props.on_change(o.value)
				end
			end,
			if has_icons then glyph(o.icon, if enabled then nil else "$text.disabled") else nil,
			if o.description
				then ui.column {
					grow = 1,
					min_w = 0,
					gap = "$zeron.space.hair",
					name,
					ui.text { o.description, text_size = "$font.size.sm", line_height = "$font.line.sm", color = "$text.tertiary", wrap = true },
				}
				else name,
			-- On the name's line, the button's chevron over it.
			ui.row { h = "$font.line.md", shrink = 0, items = "center", Menu.check(on) },
		}, tall), i, lit, rows)
		-- Its height and inset the button's, past the button's edge, so its
		-- name lands on the button's and its check on the chevron.
		if not tall then
			p.h = control.H[size] - 4
		end
		p.px = nil
		p.pl = ui.raw(control.PAD_PT[size] + 1, "a field's inset, past its 1 edge")
		p.pr = ui.raw(9, "the button's 8 end inset, past its 1 edge")
		return ui.menu_item(p)
	end

	-- The option to open over (the chosen one, else the first that can be
	-- taken), where a menu opens over the button at all.
	local function over_id(): string?
		if not shape:get().over then
			return nil
		end
		local v = value:peek()
		local first: number? = nil
		for i, o in options() do
			if o.value == v then
				return `select-option-{i}`
			end
			first = first or (if o.disabled then nil else i)
		end
		return if first then `select-option-{first}` else nil
	end

	local menu = ui.menu(Menu.panel {
		open = open,
		anchor = trigger,
		label = props.label or props.name,
		w = function(): number
			local wide = box.width or 0
			local now = shape:get()
			if now.over then
				return wide + OVERHANG * 2
			end
			return math.max(wide, if now.tall then ui.theme.get("zeron.menu.width") :: number elseif fit then FIT_MENU else 0)
		end,
		placement = { side = "bottom", align = if fit then "end" else "start", offset = 4 },
		open_over = over_id,
		-- As Zeron's menus open: a quick fade, no travel over the button.
		on_mount = function(el: Element)
			anime.animate(el, { opacity = { 0, 1 }, duration = 120, ease = "out(3)" })
		end,
		-- (Built each time it opens, from the options as they stand then.)
		build = function()
			local now = shape:peek()
			local over, tall = now.over, now.tall
			local list_now = options()
			local rows: { any } = {}
			local items: { Node } = {}
			local heading: string? = nil
			for i, o in list_now do
				-- A group's heading over its first option.
				-- (Told apart by their words: two messages are two userdata.)
				local group = control.text(o.group)
				if group and group ~= heading then
					heading = group
					table.insert(items, Menu.Heading { o.group, first = #items == 0 })
				end
				if o.rule and #items > 0 then
					table.insert(items, Menu.Rule {})
				end
				table.insert(items, row(i, o, rows, now.icons, tall))
			end
			local list = ui.column {
				w_full = true,
				p = "$space.1",
				gap = "$zeron.list.gap",
				-- Below the button zgx has no chosen row to open on: on it here.
				on_mount = function()
					if over then
						return
					end
					ui.after(0, function()
						for i, o in list_now do
							if rows[i] and (o.value == value:peek() or (value:peek() == nil and not o.disabled)) then
								rows[i]:focus()
								-- In the middle of the view, what is round it showing.
								local step = control.H[size] - 4 + GAP
								offset:set(math.max(0, (i - 1) * step - (VIEW - step) / 2))
								return
							end
						end
					end)
				end,
				table.unpack(items),
			}
			if over or tall or now.short then
				return list
			end
			return ui.scroll { w_full = true, max_h = VIEW, offset = offset, fade = { 12, 12 }, fade_overflow = true, list }
		end,
	})

	return field.wrap {
		label = props.label,
		hint = props.hint,
		error = props.error,
		size = size,
		box = menu,
		disabled = props.disabled,
		w = if props.w_full or fit then nil else (props.w or 240),
		w_full = props.w_full,
	}
end)
