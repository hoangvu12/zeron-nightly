-- The agent menu of a chat on a sandbox (spec #37, screens 04 and 19): in
-- the model chip's place, one flat menu, "Agents" (each agent, a ✓ on the
-- one that runs) over "Model" (that agent's models, a ✓ on the one picked).
-- Switching agent is one click: its own default model is picked with it,
-- and the chip then reads the agent ("Codex High", screen 17) until a model
-- is picked by name. The reasoning stays the model's own (or what was
-- picked for it elsewhere).
--
--   AgentMenu { model = state, effort = state, agent = state,
--     harnesses = …, models = … }

local Dropdown = require("@self/dropdown")
local Menu = require("@self/menu")
local AgentMark = require("@self/agent_mark")
local ModelPicker = require("@self/model_picker")
local locale = require("@self/locale")

export type Props = {
	model: State<string?>,
	effort: State<string?>,
	-- The agent picked from Agents (by id), while the chip reads it; a model
	-- picked by name clears it.
	agent: State<string?>,
	harnesses: { ModelPicker.Harness } | State<{ ModelPicker.Harness }> | () -> { ModelPicker.Harness },
	models: { ModelPicker.Model } | State<{ ModelPicker.Model }> | () -> { ModelPicker.Model },
	-- The model's name while the catalog doesn't list it (ModelPicker's).
	model_label: (() -> string?)?,
	loading: (() -> boolean)?,
	placement: { side: string?, align: string?, offset: number? }?,
	fold: (() -> number)?,
	open_with: State<number>?,
	showing: State<boolean>?,
}

local function read(given: any): any
	if type(given) == "function" then
		return given()
	elseif type(given) == "table" and given.get then
		return given:get()
	end
	return given
end

-- A model's own reasoning: the one it starts on, else its middle level.
local function own_effort(m: ModelPicker.Model): string?
	if m.effort then
		return m.effort
	end
	local levels = m.efforts or {}
	local mid = levels[math.max(1, (#levels + 1) // 2)]
	return mid and mid.value
end

-- The agent's model a switch to it picks: its default, else its first.
local function default_of(models: { ModelPicker.Model }, harness: string): ModelPicker.Model?
	local first = nil
	for _, m in models do
		if m.harness == harness then
			if m.default then
				return m
			end
			first = first or m
		end
	end
	return first
end

local function AgentMenu(props: Props): Node
	local function models(): { ModelPicker.Model }
		return read(props.models) or {}
	end
	local function harnesses(): { ModelPicker.Harness }
		return read(props.harnesses) or {}
	end
	local function current(): ModelPicker.Model?
		local picked = props.model:get()
		for _, m in models() do
			if m.value == picked then
				return m
			end
		end
		return nil
	end
	-- The agent that runs: the picked model's, else the one picked by name.
	local function running(): string?
		local m = current()
		return if m then m.harness else props.agent:get()
	end
	local function agent_row(id: string?): ModelPicker.Harness?
		for _, h in harnesses() do
			if h.id == id then
				return h
			end
		end
		return nil
	end
	local function set_effort(m: ModelPicker.Model)
		local e = props.effort:peek()
		for _, l in m.efforts or {} do
			if l.value == e then
				return
			end
		end
		props.effort:set(own_effort(m))
	end

	-- The running agent's models, each in its mark.
	local items = ui.computed(function(): { Dropdown.Item }
		local out: { Dropdown.Item } = {}
		local agent = agent_row(running())
		for _, m in models() do
			if agent and m.harness == agent.id then
				table.insert(out, {
					value = m.value,
					label = m.label,
					mark = if agent.icon then nil else agent.mark,
					svg = agent.icon,
					svg_seed = m.harness,
				})
			end
		end
		return out
	end)

	-- Agents: one row each, a ✓ on the one that runs.
	local agent_list = ui.computed(function(): { ModelPicker.Harness }
		return harnesses()
	end)
	local agents = Menu.Group {
		heading = locale.t("ds.composer.agent_menu.agents") :: any,
		ui.each(agent_list, function(h: ModelPicker.Harness): Node
			return Menu.Row {
				id = `agent-menu-{h.id}`,
				class = "agent-menu-agent",
				lead = AgentMark { label = h.label, seed = h.id, mark = if h.icon then nil else h.mark, icon = h.icon, size = 16, bare = true },
				label = h.label,
				role = "menuitemradio",
				checked = function(): boolean
					return running() == h.id
				end,
				on_click = function()
					if running() == h.id and props.agent:peek() == h.id then
						return
					end
					local m = default_of(models(), h.id)
					if m then
						props.model:set(m.value)
						set_effort(m)
					end
					props.agent:set(h.id)
				end,
			}
		end, {
			key = function(h: ModelPicker.Harness): string
				return h.id
			end,
		}),
	}

	return Dropdown {
		fold = props.fold,
		-- (Only with nothing to show yet, as ModelPicker's.)
		loading = if props.loading
			then function(): boolean
				return (props.loading :: () -> boolean)() and current() == nil and (props.model_label == nil or props.model_label() == nil)
			end
			else nil,
		value = props.model,
		items = items :: any,
		tone = "chip",
		mark = true,
		-- The agent's name while it was picked from Agents (screen 17); else
		-- the model's, or the host's name for one not listed.
		say = function(): string?
			local picked = props.agent:get()
			local m = current()
			if picked and (m == nil or m.harness == picked) then
				local h = agent_row(picked)
				if h then
					return h.label
				end
			end
			if m == nil and props.model_label then
				return props.model_label()
			end
			return nil
		end,
		detail = function(): string
			local m = current()
			local e = props.effort:get()
			for _, l in (m and m.efforts) or {} do
				if l.value == e then
					return l.label
				end
			end
			return ""
		end,
		roll_in_panel = true,
		label = locale.t("ds.model_picker.model") :: any,
		search = false,
		check = true,
		width = 232,
		head = agents,
		list_heading = locale.t("ds.composer.agent_menu.model") :: any,
		on_choose = function(value: string)
			-- A model picked by name: the chip reads it.
			props.agent:set(nil)
			for _, m in models() do
				if m.value == value then
					set_effort(m)
				end
			end
		end,
		placement = props.placement,
		open_with = props.open_with,
		showing = props.showing,
	}
end

return AgentMenu
