-- Settings › Sandboxes, its words and choices, pure (no `ui` needed: a test
-- reads the English). A step script is named by `{for, hook, name}`, as the
-- engine's `ListStepScripts`/`WriteStepScript` name it (crates/engine
-- sandboxes/scripts.rs): `for` is `{kind = "all"}`, `{kind = "provider",
-- provider}` or `{kind = "project", spaceId}`; `hook` is `before-`,
-- `after-` or `instead-of-` and a step, or `setup` for a project's own
-- `.zeron/setup.sh`.
--
-- The page asks When as five moments and "Replace a step…", never as the
-- grid of hooks: a script on another hook (Fleet-synced, written by hand)
-- still lists, its When said plainly ("Before Install Zeron").

local M = {}

local function say(key: string, english: string, args: { [string]: any }?): string
	if (ui :: any) == nil then
		if args then
			return (string.gsub(english, "{(%w+)}", function(k: string): string
				return tostring(args[k] or "")
			end))
		end
		return english
	end
	return ui.t(`settings.sandboxes.{key}`, args):get()
end

export type Scope = { kind: string, provider: string?, spaceId: string? }
export type Script = { ["for"]: Scope, hook: string, name: string, script: string?, path: string?, runsOn: string? }
export type Provider = { id: string, name: string, icon: string?, account: { name: string }?, host: string? }
export type Project = { id: string, name: string, device: string? }

-- A project's own Setup script (`<repo>/.zeron/setup.sh`).
M.SETUP_HOOK = "setup"
M.SETUP_NAME = "setup.sh"

-- When, as the page offers it, in order: each a moment's hook.
M.MOMENTS = {
	{ value = "after-create", key = "when.created", english = "When it's created" },
	{ value = "after-ready", key = "when.ready", english = "When it's ready" },
	{ value = "after-wake", key = "when.wakes", english = "When it wakes" },
	{ value = "before-sleep", key = "when.sleeps", english = "Before it sleeps" },
	{ value = "before-delete", key = "when.deleted", english = "Before it's deleted" },
}
-- The last choice: a step to run instead of (then which step).
M.REPLACE = "replace"

-- The steps, in the order they run, with their names (ready isn't one to
-- replace: it's only the moment the others are done).
M.STEPS = {
	{ value = "create", key = "step.create", english = "Create" },
	{ value = "start", key = "step.start", english = "Start" },
	{ value = "install-zeron", key = "step.install_zeron", english = "Install Zeron" },
	{ value = "sign-in", key = "step.sign_in", english = "Sign in" },
	{ value = "secrets", key = "step.secrets", english = "Secrets" },
	{ value = "get-code", key = "step.get_code", english = "Get the code" },
	{ value = "setup", key = "step.setup", english = "Setup" },
	{ value = "ready", key = "step.ready", english = "Ready" },
	{ value = "sleep", key = "step.sleep", english = "Sleep" },
	{ value = "wake", key = "step.wake", english = "Wake" },
	{ value = "delete", key = "step.delete", english = "Delete" },
}

local function step_named(step: string): string?
	for _, s in M.STEPS do
		if s.value == step then
			return say(s.key, s.english)
		end
	end
	return nil
end

local function moment(hook: string): any?
	for _, m in M.MOMENTS do
		if m.value == hook then
			return m
		end
	end
	return nil
end

-- When's choices: the moments, then Replace a step… set apart.
function M.when_options(): { any }
	local out = {}
	for _, m in M.MOMENTS do
		table.insert(out, { value = m.value, label = say(m.key, m.english) })
	end
	table.insert(out, { value = M.REPLACE, label = say("when.replace", "Replace a step…") })
	return out
end

-- The steps a script can stand in for.
function M.step_options(): { any }
	local out = {}
	for _, s in M.STEPS do
		if s.value ~= "ready" then
			table.insert(out, { value = s.value, label = say(s.key, s.english) })
		end
	end
	return out
end

-- The hook a When choice names (`step`: the step, for Replace).
function M.hook_of(choice: string, step: string?): string?
	if choice == M.REPLACE then
		return if step and step_named(step) then `instead-of-{step}` else nil
	end
	return choice
end

-- A hook as the When choices hold it: `(choice, step?)`. A project's
-- setup.sh is when it's ready; a hook that isn't one of the moments is
-- itself (listed as it is).
function M.choice_of(hook: string): (string, string?)
	if hook == M.SETUP_HOOK then
		return "after-ready", nil
	end
	local step = string.match(hook, "^instead%-of%-(.+)$")
	if step then
		return M.REPLACE, step
	end
	return hook, nil
end

-- When a script runs, in words: "When it wakes", "Instead of Get the
-- code", "Before Install Zeron".
function M.when(hook: string): string
	if hook == M.SETUP_HOOK then
		return say("when.ready", "When it's ready")
	end
	local m = moment(hook)
	if m then
		return say(m.key, m.english)
	end
	for _, timing in { { "instead-of-", "when.instead_of", "Instead of {step}" }, { "before-", "when.before", "Before {step}" }, { "after-", "when.after", "After {step}" } } do
		local prefix, key, english = timing[1], timing[2], timing[3]
		if string.sub(hook, 1, #prefix) == prefix then
			local step = step_named(string.sub(hook, #prefix + 1))
			if step then
				return say(key, english, { step = step })
			end
		end
	end
	return hook
end

local function find(list: { any }?, field: string, value: string?): any?
	for _, item in list or {} do
		if item[field] == value then
			return item
		end
	end
	return nil
end

-- Who a script is for: its words and its glyph.
function M.for_label(scope: Scope, providers: { Provider }?, projects: { Project }?): (string, string)
	if scope.kind == "provider" then
		local p = find(providers, "id", scope.provider)
		return if p then p.name else (scope.provider or ""), M.icon_of(p and p.icon)
	elseif scope.kind == "project" then
		local p = find(projects, "id", scope.spaceId)
		return if p then p.name else say("for.project_gone", "A removed project"), "folder"
	end
	return say("for.all", "All sandboxes"), "widget"
end

-- A provider's glyph: its own where Zeron's icons have it (the inlined set,
-- ds/icon_svgs), else a cloud.
local got, SVGS = pcall(require, "@ds/icon_svgs")
local GLYPHS: { [string]: any } = if got and type(SVGS) == "table" then SVGS else { boat = true, cloud = true }
function M.icon_of(icon: string?): string
	return if icon and GLYPHS[icon] then icon else "cloud"
end

-- A script's identity: one row each, whatever is renamed around it.
function M.key(s: Script): string
	local scope = s["for"]
	local whom = if scope.kind == "provider" then `provider:{scope.provider}` elseif scope.kind == "project" then `project:{scope.spaceId}` else "all"
	return `{whom}/{s.hook}/{s.name}`
end

export type Row = { key: string, name: string, when: string, whom: string, icon: string, script: Script }

-- The page's list: one row per script there is (no empty moments, no
-- grid), in the engine's order.
function M.rows(scripts: { Script }?, providers: { Provider }?, projects: { Project }?): { Row }
	local out: { Row } = {}
	for _, s in scripts or {} do
		local whom, icon = M.for_label(s["for"], providers, projects)
		table.insert(out, { key = M.key(s), name = s.name, when = M.when(s.hook), whom = whom, icon = icon, script = s })
	end
	return out
end

-- For's choices: every sandbox, each provider, each project with its folder
-- on the host device (a script's file lives there).
function M.for_options(providers: { Provider }?, projects: { Project }?, host: string?): { any }
	local out = { { value = "all", label = say("for.all", "All sandboxes"), icon = "widget" } }
	for _, p in providers or {} do
		table.insert(out, { value = `provider:{p.id}`, label = p.name, icon = M.icon_of(p.icon) })
	end
	for _, p in projects or {} do
		if host == nil or p.device == host then
			table.insert(out, { value = `project:{p.id}`, label = p.name, icon = "folder" })
		end
	end
	return out
end

-- A For choice's value, as the engine names it.
function M.scope_of(value: string): Scope
	local provider = string.match(value, "^provider:(.+)$")
	if provider then
		return { kind = "provider", provider = provider }
	end
	local project = string.match(value, "^project:(.+)$")
	if project then
		return { kind = "project", spaceId = project }
	end
	return { kind = "all" }
end

-- A scope as a For choice's value.
function M.scope_value(scope: Scope): string
	if scope.kind == "provider" then
		return `provider:{scope.provider}`
	elseif scope.kind == "project" then
		return `project:{scope.spaceId}`
	end
	return "all"
end

-- A script's file name: one plain name (no folders, not hidden).
function M.valid_name(name: string): boolean
	return name ~= "" and name ~= "." and name ~= ".." and string.sub(name, 1, 1) ~= "." and not string.find(name, "[/\\:%z]")
end

-- Whether the host device sleeps: a computer that isn't an always-on
-- `zeron headless` (a laptop, a desktop that's put to sleep).
function M.sleeps(device: { platform: string, headless: boolean? }?): boolean
	if not device then
		return false
	end
	local phone = device.platform == "ios" or device.platform == "android"
	return device.headless ~= true and not phone
end

-- The device the scripts are on: the host device of the first provider
-- that has one, else this one (`here`).
function M.scripts_device(providers: { Provider }?, here: string?): string?
	for _, p in providers or {} do
		if p.host then
			return p.host
		end
	end
	return here
end

return M
