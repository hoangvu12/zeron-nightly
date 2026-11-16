-- The composer's band: one row of its own at the composer's top, inside its
-- frame (its rounded top corners are the composer's), a rule under it, over
-- the deck and the pill. It's always shown while it has something to say,
-- never folded into the deck's alerts: a place's state the person reads at
-- a glance (spec #37 story 32: a sandbox starting, waking, asleep, a step
-- or script that failed, deleted; an agent not on its template; saved as a
-- template).
--
-- A row: its glyph (the working ring while `spinner`) in its tone's ink,
-- its title, a muted word after it (`mono`: a path or a name, in the code
-- font), how long (muted, tabular), the steps as segments under the title
-- (`progress`: each step's state, the one running in the accent), then its
-- action (a quiet text button in the accent) and its × (`dismissable`).
--
--   Band { band = fn_or_state, on_action = fn, on_dismiss = fn }

local Icon = require("@self/icon")
local FadeText = require("@self/fade_text")
local IconButtonModule = require("@self/icon_button")
local IconButton = IconButtonModule.IconButton
local status = require("@self/status")
local control = require("@self/control")
local locale = require("@self/locale")

local M = {}

export type Band = {
	kind: string?,
	title: string,
	icon: string?,
	spinner: boolean?,
	-- "danger", "warning", "success", "info": its glyph's ink; none, the
	-- secondary text's.
	tone: string?,
	detail: string?,
	mono: boolean?,
	time: string?,
	progress: { string }?,
	action: string?,
	dismissable: boolean?,
}

export type Props = {
	band: State<Band?> | () -> Band?,
	on_action: (() -> ())?,
	on_dismiss: (() -> ())?,
}

-- The row's height, and a segment's.
M.ROW_H = 36
local SEGMENT_W, SEGMENT_H, SEGMENT_GAP = 18, 2, 3

local function read(b: State<Band?> | () -> Band?): Band?
	if type(b) == "function" then
		return (b :: () -> Band?)()
	end
	return (b :: State<Band?>):get()
end

-- A step's segment: done (and skipped) in the secondary ink, the one
-- running (or failed) in the accent (the danger's), the rest faint.
local function segment_ink(state: string): string
	if state == "done" or state == "skipped" then
		return "$text.tertiary"
	elseif state == "running" then
		return "$accent"
	elseif state == "failed" then
		return "$danger"
	end
	return "$border.subtle"
end

local function segments(band: () -> Band?): Node
	-- (By place: a segment stays as its state moves on.)
	local steps = ui.computed(function(): { { i: number, state: string } }
		local b = band()
		local out = {}
		for i, s in (b and b.progress) or {} do
			table.insert(out, { i = i, state = s })
		end
		return out
	end)
	return ui.show(function(): boolean
		local b = band()
		return b ~= nil and b.progress ~= nil and #b.progress > 0
	end, ui.row {
		class = "composer-band-progress",
		gap = ui.raw(SEGMENT_GAP, "between a step's segments"),
		aria_hidden = true,
		ui.each(steps, function(s: { i: number, state: string })
			return ui.div {
				class = `composer-band-segment-{s.state}`,
				w = ui.raw(SEGMENT_W, "a step's segment"),
				h = ui.raw(SEGMENT_H, "a step's segment"),
				rounded = ui.raw(1, "a segment's ends"),
				bg = function(): string
					local b = band()
					local now = b and b.progress and b.progress[s.i]
					return segment_ink(now or s.state)
				end,
			}
		end, {
			key = function(s: { i: number, state: string })
				return tostring(s.i)
			end,
		}),
	})
end

function M.Band(props: Props): Node
	local function band(): Band?
		return read(props.band)
	end
	local function ink(): string
		local b = band()
		local tone = b and b.tone
		return if tone then `$zeron.tone.{tone}.ink` else "$text.secondary"
	end
	local glyph = ui.div {
		w = 18,
		h = 18,
		shrink = 0,
		items = "center",
		justify = "center",
		color = ink,
		ui.show(function(): boolean
			local b = band()
			return b ~= nil and b.spinner == true
		end, status.StatusIcon { kind = "working", size = 14 }, Icon {
			name = function(): Icon.Name
				local b = band()
				return ((b and b.icon) or "info-circle") :: Icon.Name
			end,
			size = 16,
		}),
	}
	-- The word after the title (a path or a name in the code font).
	local function detail(font: string?): Node
		return FadeText {
			function(): string
				local b = band()
				return (b and b.detail) or ""
			end,
			id = "composer-band-detail",
			literal = true,
			shrink = 1,
			min_w = 0,
			font = font,
			text_size = "$font.size.sm",
			color = "$text.tertiary",
		}
	end
	local line = ui.row {
		w_full = true,
		min_w = 0,
		gap = "$space.2",
		items = "baseline",
		FadeText {
			function(): string
				local b = band()
				return if b then b.title else ""
			end,
			id = "composer-band-title",
			literal = true,
			shrink = 0,
			max_w = "70%",
			text_size = "$font.size.md",
			weight = "$font.weight.medium",
			color = "$text.primary",
		},
		ui.show(function(): boolean
			local b = band()
			return b ~= nil and b.detail ~= nil and b.detail ~= ""
		end, ui.show(function(): boolean
			local b = band()
			return b ~= nil and b.mono == true
		end, detail("$font.mono"), detail(nil))),
		ui.show(function(): boolean
			local b = band()
			return b ~= nil and b.time ~= nil
		end, ui.text {
			function(): string
				local b = band()
				return (b and b.time) or ""
			end,
			id = "composer-band-time",
			shrink = 0,
			tabular = true,
			text_size = "$font.size.sm",
			color = "$text.tertiary",
		}),
	}
	local action = ui.show(function(): boolean
		local b = band()
		return b ~= nil and b.action ~= nil and props.on_action ~= nil
	end, ui.button {
		h = 24,
		px = "$space.2",
		shrink = 0,
		items = "center",
		rounded = ui.raw(12, "a quiet capsule"),
		cursor = "pointer",
		role = "button",
		label = function(): string
			local b = band()
			return (b and b.action) or ""
		end,
		color = "$accent",
		hover = { bg = "$hover" },
		active = { bg = "$pressed" },
		focus = control.ring,
		on_click = function()
			if props.on_action then
				props.on_action()
			end
		end,
		ui.text {
			function(): string
				local b = band()
				return (b and b.action) or ""
			end,
			text_size = "$font.size.sm",
			weight = "$font.weight.medium",
		},
	})
	local dismiss = ui.show(function(): boolean
		local b = band()
		return b ~= nil and b.dismissable == true and props.on_dismiss ~= nil
	end, IconButton {
		icon = "close",
		label = function(): string
			local b = band()
			return locale.s("ds.composer_dock.dismiss_alert", { title = if b then b.title else "" })
		end,
		size = "sm",
		tone = "quiet",
		on_click = function()
			if props.on_dismiss then
				props.on_dismiss()
			end
		end,
	})
	-- (Keyed by its kind: another kind is another row, faded in.)
	return ui.presence(function(): string?
		local b = band()
		return if b then (b.kind or "note") else nil
	end, function(kind: string): Node
		return ui.column {
			id = "composer-band",
			class = `composer-band composer-band-{kind}`,
			role = "status",
			label = function(): string
				local b = band()
				return if b then b.title else ""
			end,
			w_full = true,
			shrink = 0,
			ui.row {
				w_full = true,
				min_h = M.ROW_H,
				pl = "$space.3",
				pr = "$space.2",
				py = "$space.1",
				gap = "$space.2",
				items = "center",
				glyph,
				ui.column {
					grow = 1,
					basis = 0,
					min_w = 0,
					gap = "$space.1",
					line,
					segments(band),
				},
				action,
				dismiss,
			},
			-- The rule between it and what's under it (the deck, the pill),
			-- edge to edge, as the deck's.
			ui.div { h = 1, w_full = true, shrink = 0, bg = "$border.subtle", aria_hidden = true },
		}
	end, {
		w_full = true,
		enter = { opacity = { 0, 1 }, duration = 180, ease = "out(3)" },
		exit = { opacity = 0, duration = 120 },
	})
end

return M
