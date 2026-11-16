-- Settings › Sandboxes (spec #37, ticket #48): one page for every sandbox
-- provider. Each provider is a card: whether it's connected, with Connect
-- or Disconnect, and its host device (the device that keeps its key and
-- runs its steps), with a note when that device may sleep. Not connected
-- yet, the host device is the one it will get (picked there, it moves there
-- once connected). Under them, the user's step scripts as a flat list, one
-- row per script that exists (its name, When, For), Add script, and an
-- editor; connected or not, the page shows all of it.
--
-- Everything comes from the engine (src/sandbox_settings.rs): the
-- providers here (`ListSandboxProviders`: their ops too, which say which
-- sign in on their own page), each one's host device (`SandboxHost`), and
-- who it's connected as, which only the host device says (asked there).
-- Step scripts are files on the host devices, but every script call goes to
-- this device's engine, which routes it to the right one(s). Read once a
-- visit, again after each change (no polling), but while a provider's own
-- sign-in page waits on the user.
--
-- No descriptions on the page: the rows say what they are.

local ds = require("@ds")
local Icon = require("@ds/icon")
local Links = require("@ds/links")
local CodeEditor = require("@ds/code_editor")
local Syntax = require("@ds/syntax")
local toast = require("@ds/toast")
local sandboxes = require("@account/sandboxes")
local sessions = require("@account/sessions")
local host = require("@app/screens/settings/host")
local search = require("@app/screens/settings/search")
local visits = require("@app/screens/settings/visits")
local words = require("@app/screens/settings/sandbox_words")

local M = {}

M.ID = "sandboxes"
M.TITLE = "Sandboxes"

search.index(M.ID, ui.t("settings.sandboxes.title"), ui.t("settings.sandboxes.terms"), {
	{ key = "providers", title = ui.t("settings.sandboxes.connect"), terms = ui.t("settings.sandboxes.providers_terms") },
	{ key = "providers", title = ui.t("settings.sandboxes.host_device"), terms = ui.t("settings.sandboxes.host_terms") },
	{ key = "scripts", title = ui.t("settings.sandboxes.scripts"), terms = ui.t("settings.sandboxes.scripts_terms") },
})

type Provider = words.Provider
type Script = words.Script

-- What the page shows: "loading", "ready", or "offline" (no engine).
local phase = ui.state("settings_sandboxes_phase", "loading")
-- What the engine says of each provider, by id: its host device, who it's
-- connected as, what it can do, and (one the picker doesn't list yet) its
-- name and icon.
type Said = { id: string?, name: string?, icon: string?, host: string?, account: { name: string }?, ops: { string }? }
local said = ui.state("settings_sandboxes_said", {} :: { [string]: Said })

-- The providers, as the picker lists them (account/sandboxes: their names,
-- order and glyphs), each with what its engine says.
local function merged(of: { [string]: Said }): { Provider }
	local out: { Provider } = {}
	local listed: { [string]: boolean } = {}
	for _, p in sandboxes.providers() do
		listed[p.id] = true
		local s = of[p.id] or {}
		table.insert(out, { id = p.id, name = p.name, icon = sandboxes.icon(p.id), host = s.host, account = s.account, ops = s.ops })
	end
	local rest: { Provider } = {}
	for id, s in of do
		if not listed[id] then
			table.insert(rest, { id = id, name = s.name or id, icon = sandboxes.icon(s.icon or id), host = s.host, account = s.account, ops = s.ops })
		end
	end
	table.sort(rest, function(a: Provider, b: Provider): boolean
		return a.id < b.id
	end)
	table.move(rest, 1, #rest, #out + 1, out)
	return out
end
local providers = ui.computed(function(): { Provider }
	return merged(said:get())
end)
local scripts = ui.state("settings_sandboxes_scripts", {} :: { Script })
-- A script just added: its row's key, lit for a moment.
local fresh = ui.state("settings_sandboxes_fresh", "")
-- How long a new script's row stays lit.
local FRESH_MS = 1600
-- The host device picked for a provider not connected yet, by id: where it
-- moves once it's connected.
local planned = ui.state("settings_sandboxes_planned", {} :: { [string]: string })

-- Connecting: which provider, the key being pasted, the provider's sign-in
-- page's code while it waits, and whether that's on its way.
local connecting = ui.state("settings_sandboxes_connecting", "")
local connect_open = ui.state("settings_sandboxes_connect_open", false)
local token = ui.state("settings_sandboxes_token", "")
local code = ui.state("settings_sandboxes_code", nil :: any)
local connect_busy = ui.state("settings_sandboxes_connect_busy", false)
-- Disconnecting: which, after a confirm.
local disconnecting = ui.state("settings_sandboxes_disconnecting", "")
local disconnect_open = ui.state("settings_sandboxes_disconnect_open", false)
local disconnect_busy = ui.state("settings_sandboxes_disconnect_busy", false)
-- The editor: the script opened (nil: a new one), its When (a moment, or
-- Replace and a step), For, file name and text.
local editor_open = ui.state("settings_sandboxes_editor_open", false)
local editing = ui.state("settings_sandboxes_editing", nil :: Script?)
local when = ui.state("settings_sandboxes_when", "after-ready")
local step = ui.state("settings_sandboxes_step", "")
local whom = ui.state("settings_sandboxes_for", "all")
local file_name = ui.state("settings_sandboxes_file", "")
local content = ui.state("settings_sandboxes_content", "")
local editor_busy = ui.state("settings_sandboxes_editor_busy", false)
local delete_open = ui.state("settings_sandboxes_delete_open", false)

-- The script editor's code, this tall (about seven lines; it scrolls past).
local EDITOR_H = 160
-- How often a sign-in on the provider's page is looked at while it waits.
local POLL_MS = 2500

local function here(): string?
	for _, d in sessions.devices:peek() or {} do
		if d.here then
			return d.id
		end
	end
	return nil
end

-- Another device's id, or nil for this one (the call stays here).
local function target(device: string?): string?
	return if device and device ~= "" and device ~= here() then device else nil
end

local function ask(method: string, params: any?, device: string?): any
	return host.call("sandbox_settings.ask", { method = method, params = params or {}, device = target(device) })
end

local function change(method: string, params: any?, device: string?): any
	return host.call("sandbox_settings.change", { method = method, params = params or {}, device = target(device) })
end

-- Why something didn't go, in words: the engine's sentence, or a word's.
local function why(err: any): string
	local said = if type(err) == "string" then err elseif type(err) == "table" or type(err) == "userdata" then err.message else nil
	if said == "offline" then
		return ui.t("settings.engine.offline"):get()
	elseif said == "outdated" then
		return ui.t("settings.engine.outdated"):get()
	elseif said == nil or said == "" or said == "failed" or said == "refused" then
		return ui.t("settings.sandboxes.try_again"):get()
	end
	return said
end

local function failed(title: string): (err: any) -> ()
	return function(err: any)
		toast.show { title = title, body = why(err), tone = "danger" }
	end
end

local function device_named(id: string?): sessions.Device?
	for _, d in sessions.devices:peek() or {} do
		if d.id == id then
			return d
		end
	end
	return nil
end

-- The scripts, wherever they are: this device's engine asks the host
-- devices.
local function load_scripts()
	ask("ListStepScripts"):after(function(list: any)
		scripts:set(if type(list) == "table" then list else {})
	end, function()
		scripts:set({})
	end)
end

-- Everything again: each provider's host device and account, then the
-- scripts.
local function load()
	ask("ListSandboxProviders"):after(function(list: any)
		local found: { [string]: Said } = {}
		for _, p in (if type(list) == "table" then list else {}) do
			found[p.id] = { name = p.name, icon = p.icon, account = p.account, ops = if type(p.ops) == "table" then p.ops else nil }
		end
		for _, p in sandboxes.providers() do
			found[p.id] = found[p.id] or {}
		end
		local left = 0
		for _ in found do
			left += 1
		end
		local function done()
			left -= 1
			if left > 0 then
				return
			end
			ui.batch(function()
				said:set(found)
				phase:set("ready")
			end)
			load_scripts()
		end
		if left == 0 then
			left = 1
			done()
			return
		end
		for id, p in found do
			p.id = id
			ask("SandboxHost", { provider = p.id }):after(function(named: any)
				p.host = if type(named) == "table" then named.hostDeviceId else nil
				-- Who it's connected as is the host device's to say.
				if p.host and target(p.host) then
					ask("ListSandboxProviders", {}, p.host):after(function(there: any)
						p.account = nil
						for _, q in (if type(there) == "table" then there else {}) do
							if q.id == p.id then
								p.account = q.account
							end
						end
						done()
					end, function()
						-- (The host device away: connected, as far as anyone can tell.)
						done()
					end)
				else
					done()
				end
			end, function()
				done()
			end)
		end
	end, function(err: any)
		local said = if type(err) == "table" or type(err) == "userdata" then err.message else err
		phase:set(if said == "offline" then "offline" else "ready")
	end)
end
M.load = load

local function provider(id: string): Provider?
	for _, p in providers:peek() do
		if p.id == id then
			return p
		end
	end
	return nil
end

-- Connect ──────────────────────────────────────────────────────────────────

local function open_connect(id: string)
	ui.batch(function()
		connecting:set(id)
		token:set("")
		code:set(nil)
		connect_busy:set(false)
		connect_open:set(true)
	end)
end

-- `id` connected, `host` the host device the engine gave it: moved to the
-- one picked before, if another was.
local function connected(id: string, host: string?)
	local want = planned:peek()[id]
	ui.batch(function()
		connect_busy:set(false)
		connect_open:set(false)
		code:set(nil)
		token:set("")
		if want then
			planned:set_key(id, nil :: any)
		end
	end)
	if want and host and want ~= host then
		change("SetSandboxHost", { provider = id, deviceId = want }):after(function()
			load()
		end, function(err: any)
			failed(ui.t("settings.sandboxes.couldnt_move"):get())(err)
			load()
		end)
		return
	end
	load()
end

local function connect_with_token()
	local id = connecting:peek()
	local key = string.match(token:peek(), "^%s*(.-)%s*$") or ""
	if id == "" or key == "" or connect_busy:peek() then
		return
	end
	connect_busy:set(true)
	change("ConnectSandboxProvider", { provider = id, token = key }):after(function(answer: any)
		connected(id, if type(answer) == "table" then answer.hostDeviceId else nil)
	end, function(err: any)
		connect_busy:set(false)
		failed(ui.t("settings.sandboxes.couldnt_connect"):get())(err)
	end)
end

-- The provider's own sign-in page: its code shown, opened in the browser;
-- the engine finishes in the background, so the page asks the host device
-- every few seconds until it's connected or the code runs out.
local function connect_on_page()
	local id = connecting:peek()
	if id == "" or connect_busy:peek() then
		return
	end
	connect_busy:set(true)
	change("ConnectSandboxProvider", { provider = id }):after(function(answer: any)
		connect_busy:set(false)
		local shown = if type(answer) == "table" then answer.deviceCode else nil
		if not shown then
			connected(id, if type(answer) == "table" then answer.hostDeviceId else nil)
			return
		end
		code:set(shown)
		local url = shown.verificationUriComplete or shown.verificationUri
		if url and url ~= "" then
			Links.outside(url)
		end
		local until_at = os.time() + (tonumber(shown.expiresIn) or 900)
		-- (Only while this code shows: closed, approved or run out, it stops.)
		local function waiting(): boolean
			-- (A state holds a copy: the code's own words say which.)
			local now = code:peek()
			return connect_open:peek() and now ~= nil and now.userCode == shown.userCode and os.time() <= until_at
		end
		local function look()
			ui.after(POLL_MS, function()
				if not waiting() then
					return
				end
				ask("SandboxHost", { provider = id }):after(function(named: any)
					local device = if type(named) == "table" then named.hostDeviceId else nil
					ask("ListSandboxProviders", {}, device):after(function(list: any)
						for _, p in (if type(list) == "table" then list else {}) do
							if p.id == id and p.account and waiting() then
								connected(id, device)
								return
							end
						end
						look()
					end, look)
				end, look)
			end)
		end
		look()
	end, function(err: any)
		connect_busy:set(false)
		failed(ui.t("settings.sandboxes.couldnt_connect"):get())(err)
	end)
end

local function disconnect()
	local id = disconnecting:peek()
	if id == "" or disconnect_busy:peek() then
		return
	end
	disconnect_busy:set(true)
	change("DisconnectSandboxProvider", { provider = id }):after(function()
		ui.batch(function()
			disconnect_busy:set(false)
			disconnect_open:set(false)
		end)
		load()
	end, function(err: any)
		ui.batch(function()
			disconnect_busy:set(false)
			disconnect_open:set(false)
		end)
		failed(ui.t("settings.sandboxes.couldnt_disconnect"):get())(err)
	end)
end

-- Host device ──────────────────────────────────────────────────────────────

local function set_host(id: string, device: string)
	local p = provider(id)
	if not p or p.host == device then
		return
	end
	if p.account == nil then
		-- Not connected: where it goes once it is.
		planned:set_key(id, device)
		return
	end
	change("SetSandboxHost", { provider = id, deviceId = device }):after(function()
		load()
	end, function(err: any)
		failed(ui.t("settings.sandboxes.couldnt_move"):get())(err)
		-- (The choice goes back to where it is.)
		load()
	end)
end

-- The devices that can be a host device: computers on a Zeron that can.
local function host_choices(current: string?): { any }
	local out = {}
	for _, d in sessions.computers(sessions.devices:get(), true) do
		if d.host or d.id == current then
			table.insert(out, {
				value = d.id,
				label = d.name,
				icon = if d.headless then "remote-server" elseif d.platform == "linux" then "monitor" else "laptop",
				description = if not d.here and d.online == false then ui.t("settings.devices.offline") else nil,
			})
		end
	end
	return out
end

-- The host device a provider shows: its own once connected; before, the one
-- picked for it, else the one the engine would pick.
local function shown_host(p: Provider): string
	if p.account ~= nil and p.host then
		return p.host
	end
	return planned:get()[p.id] or p.host or words.planned_host(sessions.devices:get() :: any) or ""
end

-- A provider's card: its row (connected or not) and its host device, with a
-- note when that one may sleep.
local function provider_card(_p: Provider, _i: number, now: State<Provider>): Node
	local id = _p.id
	local function is_connected(): boolean
		return now:get().account ~= nil
	end
	local chosen = ui.use_state(shown_host(now:peek()))
	ui.effect(function()
		local h = shown_host(now:get())
		if h ~= chosen:peek() then
			chosen:set(h)
		end
	end)
	local function sleeper(): sessions.Device?
		if not is_connected() then
			return nil
		end
		local d = device_named(now:get().host)
		sessions.devices:get()
		return if words.may_sleep(d) then d else nil
	end
	local status = ui.row {
		gap = "$space.2",
		items = "center",
		ui.div {
			w = 6,
			h = 6,
			shrink = 0,
			rounded = "$radius.full",
			aria_hidden = true,
			bg = function(): string
				return if is_connected() then "$success" else "$text.tertiary"
			end,
		},
		ui.text {
			function(): string
				return if is_connected() then ui.t("settings.sandboxes.connected"):get() else ui.t("settings.sandboxes.not_connected"):get()
			end,
			text_size = "$font.size.sm",
			color = "$text.secondary",
		},
	}
	local connect = ds.Button {
		tone = "primary",
		size = "sm",
		label = ui.t("settings.sandboxes.connect_named", { name = _p.name }),
		on_click = function()
			open_connect(id)
		end,
		ui.t("settings.sandboxes.connect"),
	}
	local disconnect_button = ds.Button {
		tone = "secondary",
		size = "sm",
		label = ui.t("settings.sandboxes.disconnect_named", { name = _p.name }),
		on_click = function()
			ui.batch(function()
				disconnecting:set(id)
				disconnect_open:set(true)
			end)
		end,
		ui.t("settings.sandboxes.disconnect"),
	}
	local host_row = ds.Settings.Row {
		label = ui.t("settings.sandboxes.host_device"),
		icon = "global",
		control = ds.Select {
			value = chosen,
			name = ui.t("settings.sandboxes.host_device_named", { name = _p.name }),
			size = "sm",
			fit = true,
			options = function(): { any }
				return host_choices(chosen:get())
			end,
			on_change = function(device: string)
				set_host(id, device)
			end,
		},
	}
	local note = ui.row {
		px = ds.Settings.inset(),
		py = "$space.3",
		gap = "$space.2",
		items = "center",
		ui.div { color = "$text.tertiary", Icon { name = "moon", size = 14 } },
		ui.text {
			function(): string
				local d = sleeper()
				return if d then words.sleep_note(d.name) else ""
			end,
			text_size = "$font.size.sm",
			color = "$text.secondary",
			wrap = true,
		},
	}
	-- (One child: the rows that come and go bring their own hairlines.)
	return ds.Settings.Group {
		ui.column {
			w_full = true,
			ds.Settings.Row {
				label = _p.name,
				-- (Its glyph in the rounded tile, as the host device's.)
				icon = words.icon_of(_p.icon) :: any,
				control = ui.row {
					gap = "$space.4",
					items = "center",
					status,
					ui.show(is_connected, disconnect_button),
					ui.show(function(): boolean
						return not is_connected()
					end, connect),
				},
			},
			ds.Settings.Rule(),
			host_row,
			ui.show(function(): boolean
				return sleeper() ~= nil
			end, ui.column { w_full = true, ds.Settings.Rule(), note }),
		},
	}
end

-- Scripts ──────────────────────────────────────────────────────────────────

local function projects(): { words.Project }
	local out = {}
	for _, p in sessions.projects:get() or {} do
		table.insert(out, { id = p.id, name = p.name, device = p.device })
	end
	return out
end

local function open_editor(script: Script?)
	local choice, chosen_step = "after-ready", nil
	if script then
		choice, chosen_step = words.choice_of(script.hook)
	end
	ui.batch(function()
		editing:set(script)
		when:set(choice)
		step:set(chosen_step or "")
		whom:set(if script then words.scope_value(script["for"]) else "all")
		file_name:set(if script then script.name else "")
		content:set(if script then "" else "#!/usr/bin/env bash\nset -euo pipefail\n\n")
		editor_busy:set(false)
		editor_open:set(true)
	end)
	if script then
		editor_busy:set(true)
		ask("ReadStepScript", { ["for"] = script["for"], hook = script.hook, name = script.name }):after(function(read: any)
			ui.batch(function()
				content:set(if type(read) == "table" and read.content then read.content else "")
				editor_busy:set(false)
			end)
		end, function(err: any)
			ui.batch(function()
				editor_busy:set(false)
				editor_open:set(false)
			end)
			failed(ui.t("settings.sandboxes.couldnt_open"):get())(err)
		end)
	end
end

-- The hook the editor's When names (nil: Replace with no step yet).
local function chosen_hook(): string?
	local old = editing:peek()
	if old and old.hook == words.SETUP_HOOK then
		return words.SETUP_HOOK
	end
	return words.hook_of(when:peek(), if step:peek() ~= "" then step:peek() else nil)
end

local function can_save(): boolean
	local old = editing:get()
	when:get()
	step:get()
	if editor_busy:get() then
		return false
	end
	return chosen_hook() ~= nil and (old ~= nil or words.valid_name(string.match(file_name:get(), "^%s*(.-)%s*$") or ""))
end

local function save()
	if not can_save() then
		return
	end
	local old = editing:peek()
	local hook = chosen_hook() :: string
	local name = if old then old.name else (string.match(file_name:peek(), "^%s*(.-)%s*$") or "")
	local scope = if old and old.hook == words.SETUP_HOOK then old["for"] else words.scope_of(whom:peek())
	editor_busy:set(true)
	change("WriteStepScript", { ["for"] = scope, hook = hook, name = name, content = content:peek() }):after(function()
		-- Moved (another When or For): the old file goes.
		if old and (old.hook ~= hook or words.scope_value(old["for"]) ~= words.scope_value(scope)) then
			change("DeleteStepScript", { ["for"] = old["for"], hook = old.hook, name = old.name })
		end
		-- A new one's row is lit for a moment, so the eye finds it.
		local key = words.key({ ["for"] = scope, hook = hook, name = name })
		ui.batch(function()
			editor_busy:set(false)
			editor_open:set(false)
			if not old then
				fresh:set(key)
			end
		end)
		if not old then
			ui.after(FRESH_MS, function()
				if fresh:peek() == key then
					fresh:set("")
				end
			end)
		end
		load_scripts()
	end, function(err: any)
		editor_busy:set(false)
		failed(ui.t("settings.sandboxes.couldnt_save"):get())(err)
	end)
end

local function delete_script()
	local old = editing:peek()
	if not old then
		return
	end
	change("DeleteStepScript", { ["for"] = old["for"], hook = old.hook, name = old.name }):after(function()
		ui.batch(function()
			delete_open:set(false)
			editor_open:set(false)
		end)
		load_scripts()
	end, function(err: any)
		delete_open:set(false)
		failed(ui.t("settings.sandboxes.couldnt_delete"):get())(err)
	end)
end

-- A script's row: on the card's own fill (no raised one of its own), washed
-- under the pointer, lit for a moment when it's just been added.
local function script_row(_r: words.Row, _i: number, now: State<words.Row>): Node
	return ui.row {
		w_full = true,
		role = "button",
		label = _r.name,
		focusable = true,
		cursor = "pointer",
		px = ds.Settings.inset(),
		py = "$space.3",
		gap = "$space.3",
		items = "center",
		bg = function(): string
			return if fresh:get() == now:get().key then "$zeron.selected.flat.bg" else "#00000000"
		end,
		hover = { bg = "$hover" },
		active = { bg = "$pressed" },
		focus = { outline = "$zeron.focus.width", outline_color = "$zeron.focus.ring", outline_offset = -2 },
		on_click = function()
			open_editor(now:peek().script)
		end,
		ui.div {
			w = "36%",
			min_w = 0,
			ds.FadeText {
				function(): string
					return now:get().name
				end,
				font = "$font.mono",
				text_size = "$font.size.md",
				color = "$text.primary",
			},
		},
		ui.div {
			w = "28%",
			min_w = 0,
			ds.FadeText {
				function(): string
					return now:get().when
				end,
				text_size = "$font.size.md",
				color = "$text.secondary",
			},
		},
		ui.row {
			grow = 1,
			min_w = 0,
			gap = "$space.2",
			items = "center",
			ui.div { color = "$text.tertiary", Icon { name = (function(): any
				return now:get().icon
			end) :: any, size = 14 } },
			ds.FadeText {
				function(): string
					return now:get().whom
				end,
				grow = 1,
				text_size = "$font.size.md",
				color = "$text.secondary",
			},
		},
		ui.div { color = "$text.tertiary", Icon { name = "alt-arrow-right", size = 14 } },
	}
end

local function editor(): Node
	local function setup(): boolean
		local old = editing:get()
		return old ~= nil and old.hook == words.SETUP_HOOK
	end
	local when_select = ds.Select {
		value = when,
		label = ui.t("settings.sandboxes.when_label"),
		name = ui.t("settings.sandboxes.when_label"),
		w_full = true,
		disabled = setup,
		options = function(): { any }
			local out = words.when_options()
			-- (A script on another hook keeps it, said plainly.)
			local now = when:get()
			local listed = now == words.REPLACE
			for _, o in out do
				listed = listed or o.value == now
			end
			if not listed then
				table.insert(out, 1, { value = now, label = words.when(now) })
			end
			return out
		end,
	}
	local step_select = ds.Select {
		value = step,
		label = ui.t("settings.sandboxes.step_label"),
		name = ui.t("settings.sandboxes.step_label"),
		placeholder = ui.t("settings.sandboxes.pick_step"),
		w_full = true,
		options = words.step_options,
	}
	local for_select = ds.Select {
		value = whom,
		label = ui.t("settings.sandboxes.for_label"),
		name = ui.t("settings.sandboxes.for_label"),
		w_full = true,
		disabled = setup,
		options = function(): { any }
			return words.for_options(providers:get(), projects())
		end,
	}
	-- The file: its name over the code, as an editor's header. A new one's
	-- name is typed there; an existing one's path is said.
	local header = ui.row {
		w_full = true,
		h = 32,
		px = "$space.3",
		items = "center",
		shrink = 0,
		border_sides = { 0, 0, 1, 0 },
		border_color = "$border.subtle",
		ui.show(
			function(): boolean
				return editing:get() == nil
			end,
			ui.input {
				value = file_name,
				label = ui.t("settings.sandboxes.file_name"),
				placeholder = "warm-cache.sh",
				font = "$font.mono",
				text_size = "$font.size.sm",
				color = "$text.secondary",
				placeholder_color = "$zeron.field.placeholder",
				caret_color = "$accent",
				bg = "#00000000",
				border = 0,
				rounded = 0,
				px = 0,
				py = 0,
				grow = 1,
				min_w = 0,
				h_full = true,
				focus = { border = 0, outline = 0 },
			}
		),
		ui.show(
			function(): boolean
				return editing:get() ~= nil
			end,
			ds.FadeText {
				function(): string
					local old = editing:get()
					return if old then (old.script or old.name) else ""
				end,
				grow = 1,
				font = "$font.mono",
				text_size = "$font.size.sm",
				color = "$text.tertiary",
			}
		),
	}
	local code_box = ui.column {
		w_full = true,
		rounded = "$radius.lg",
		border = 1,
		border_color = "$zeron.field.edge",
		overflow = "hidden",
		header,
		ui.column {
			w_full = true,
			h = EDITOR_H,
			py = "$space.2",
			CodeEditor {
				value = content,
				label = ui.t("settings.sandboxes.script"):get(),
				language = function(): string?
					local old = editing:get()
					return Syntax.language(if old then old.name else (file_name:get() ~= "" and file_name:get() or "script.sh"))
				end,
				status = false,
				folding = false,
				on_save = function()
					save()
				end,
				grow = 1,
				w_full = true,
			},
		},
	}
	return ui.column {
		w_full = true,
		gap = "$space.4",
		ui.row {
			w_full = true,
			gap = "$space.3",
			ui.column { grow = 1, basis = 0, min_w = 0, when_select },
			ui.column { grow = 1, basis = 0, min_w = 0, for_select },
		},
		ui.show(function(): boolean
			return when:get() == words.REPLACE and not setup()
		end, step_select),
		code_box,
		ui.show(function(): boolean
			return editing:get() ~= nil
		end, ui.row {
			ds.Button {
				tone = "ghost",
				size = "sm",
				on_click = function()
					delete_open:set(true)
				end,
				ui.t("settings.sandboxes.delete_script"),
			},
		}),
	}
end

function M.view(): Node
	visits.once(M.ID, load)
	-- (A provider added or gone on this device's engine: read again.)
	local seen: string? = nil
	ui.effect(function()
		local ids = {}
		for _, p in sandboxes.providers() do
			table.insert(ids, p.id)
		end
		local now = table.concat(ids, ",")
		if seen ~= nil and now ~= seen then
			load()
		end
		seen = now
	end)
	local rows = ui.computed(function(): { words.Row }
		return words.rows(scripts:get(), providers:get(), projects())
	end)
	local function connecting_provider(): Provider?
		return provider(connecting:get())
	end
	return ui.column {
		w_full = true,
		items = "center",
		ds.Settings.Page {
			title = ui.t("settings.sandboxes.title"),
			search.anchor(
				"providers",
				ds.Settings.Section {
					ui.presence(function(): string
						return phase:get()
					end, function(kind: string): Node
						if kind == "loading" then
							return ui.column {
								w_full = true,
								label = ui.t("common.loading"),
								live = "polite",
								ds.Settings.Group { ds.SkeletonRow { trail = 80 }, ds.SkeletonRow { trail = 120 } },
							}
						elseif kind == "offline" then
							return ds.Callout {
								tone = "danger",
								title = ui.t("settings.sandboxes.offline_title") :: any,
								body = ui.t("settings.engine.offline") :: any,
							}
						end
						return ui.column {
							w_full = true,
							gap = "$space.4",
							ui.each(providers, provider_card, {
								key = function(p: Provider): string
									return p.id
								end,
								w_full = true,
								gap = "$space.4",
							}),
						}
					end, { w_full = true, enter = { opacity = { 0, 1 }, duration = 120 }, exit = { opacity = 0, duration = 60 }, mode = "wait" }),
				}
			),
			search.anchor(
				"scripts",
				ds.Settings.Section {
					title = ui.t("settings.sandboxes.scripts"),
					trail = ds.Button {
						tone = "ghost",
						size = "sm",
						label = ui.t("settings.sandboxes.add_script"),
						on_click = function()
							open_editor(nil)
						end,
						Icon { name = "plus", size = 14 },
						ui.t("settings.sandboxes.add_script"),
					},
					ds.Settings.Each(rows, script_row, {
						key = function(r: words.Row): string
							return r.key
						end,
					}),
				}
			),
		},
		-- Connect: a key to paste, or (Docker) its own sign-in page.
		ds.Dialog {
			open = connect_open,
			title = (function(): string
				local p = connecting_provider()
				return ui.t("settings.sandboxes.connect_title", { name = if p then p.name else "" }):get()
			end) :: any,
			size = "sm",
			confirm = {
				label = ui.t("settings.sandboxes.connect"),
				keep_open = true,
				busy = function(): boolean
					return connect_busy:get() and code:get() == nil
				end,
				on_click = connect_with_token,
			},
			ui.column {
				w_full = true,
				gap = "$space.3",
				ui.show(function(): boolean
					return words.signs_in_on_page(connecting_provider()) and code:get() == nil
				end, ds.Button {
					tone = "secondary",
					w_full = true,
					label = (function(): string
						local p = connecting_provider()
						return ui.t("settings.sandboxes.sign_in_on_page", { name = if p then p.name else "" }):get()
					end) :: any,
					on_click = connect_on_page,
					ui.text {
						function(): string
							local p = connecting_provider()
							return ui.t("settings.sandboxes.sign_in_on_page", { name = if p then p.name else "" }):get()
						end,
					},
				}),
				ui.show(function(): boolean
					return code:get() ~= nil
				end, ui.column {
					w_full = true,
					gap = "$space.1",
					items = "center",
					ui.text {
						function(): string
							local c = code:get()
							return if c then c.userCode or "" else ""
						end,
						font = "$font.mono",
						text_size = "$font.size.xl",
						weight = "$font.weight.semibold",
						color = "$text.primary",
						selectable = true,
					},
					ui.text { ui.t("settings.sandboxes.code_waiting"), text_size = "$font.size.sm", color = "$text.secondary" },
				}),
				ds.TextField {
					value = token,
					label = ui.t("settings.sandboxes.token"),
					name = ui.t("settings.sandboxes.token"),
					password = true,
					w_full = true,
					on_commit = function()
						connect_with_token()
					end,
				},
				-- Which kinds of key it takes, where it takes more than one.
				ui.show(function(): boolean
					return words.token_hint(connecting:get()) ~= nil
				end, ui.text {
					function(): string
						return words.token_hint(connecting:get()) or ""
					end,
					text_size = "$font.size.sm",
					line_height = "$font.line.sm",
					color = "$text.tertiary",
					wrap = true,
					selectable = true,
				}),
			},
		},
		ds.Dialog {
			open = disconnect_open,
			title = (function(): string
				local p = provider(disconnecting:get())
				return ui.t("settings.sandboxes.disconnect_title", { name = if p then p.name else "" }):get()
			end) :: any,
			body = ui.t("settings.sandboxes.disconnect_body"),
			tone = "danger",
			size = "sm",
			confirm = {
				label = ui.t("settings.sandboxes.disconnect"),
				keep_open = true,
				busy = function(): boolean
					return disconnect_busy:get()
				end,
				on_click = disconnect,
			},
		},
		ds.Dialog {
			open = editor_open,
			title = (function(): string
				local old = editing:get()
				return if old then old.name else ui.t("settings.sandboxes.add_script"):get()
			end) :: any,
			size = "lg",
			confirm = {
				label = ui.t("settings.sandboxes.save"),
				keep_open = true,
				busy = function(): boolean
					return editor_busy:get()
				end,
				on_click = save,
			},
			editor(),
		},
		ds.Dialog {
			open = delete_open,
			title = (function(): string
				local old = editing:get()
				return ui.t("settings.sandboxes.delete_title", { name = if old then old.name else "" }):get()
			end) :: any,
			tone = "danger",
			size = "sm",
			confirm = {
				label = ui.t("settings.sandboxes.delete_script"),
				on_click = delete_script,
			},
		},
	}
end

return M
