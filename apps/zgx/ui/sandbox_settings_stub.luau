-- Settings › Sandboxes' engine, answered here as the engine would
-- (src/sandbox_settings.rs: `sandbox_settings.ask` and `.change`, each
-- `{method, params, device?}`). ui/settings_stub.luau sends those two here.
--
-- Two providers, Docker Sandbox (signs in on its own page, or takes a
-- pasted token) and Boat (a pasted key), neither connected. A connect makes
-- devbox (the fixture's always-on server) the host device, as the engine
-- picks; the account then shows only in devbox's answer. Step scripts are
-- files on a device: devbox has three (one for every sandbox, one for
-- Docker's, Borealis's own setup.sh).
--
-- What was asked, in order, is `sandbox_settings_stub_asked` ("Method@device",
-- "" for this one). Commands:
--   sandbox_settings_stub.connected  Docker connected, devbox its host device
--   sandbox_settings_stub.approve    the Docker sign-in page's code approved

local M = {}

local calls = ui.state("sandbox_settings_stub_asked", {} :: { string })
local last_write = ui.state("sandbox_settings_stub_last_write", "")
local last_content = ui.state("sandbox_settings_stub_last_content", "")

local SERVER = "box"
local providers = {
	{ id = "docker", name = "Docker Sandbox", icon = "docker", ops = { "create", "connect" } },
	{ id = "boat", name = "Boat", icon = "boat", ops = { "create", "connect" } },
}
-- Who each is connected as, and its host device.
local accounts: { [string]: any } = {}
local hosts: { [string]: string } = {}
-- A Docker sign-in waiting on its page.
local pending: string? = nil

local function script(scope: any, hook: string, name: string, shown: string, content: string): any
	return { row = { ["for"] = scope, hook = hook, name = name, script = shown, path = `/home/zeron/{shown}`, runsOn = "sandbox" }, content = content }
end
local files: { [string]: { any } } = {
	[SERVER] = {
		script({ kind = "all" }, "after-ready", "fleet-sync.sh", "steps/after-ready/fleet-sync.sh", "#!/usr/bin/env bash\nfleet sync\n"),
		script({ kind = "provider", provider = "docker" }, "after-wake", "git-fetch.sh", "steps/docker/after-wake/git-fetch.sh", "git fetch --all\n"),
		script({ kind = "project", spaceId = "borealis" }, "setup", "setup.sh", "borealis/.zeron/setup.sh", "pnpm install\n"),
	},
}

-- The picker's list (account/sandboxes, src/sandboxes.rs publishes it), as
-- this device's engine has them.
local shared = require("@account/sandboxes")
if #shared.providers() == 0 then
	local listed = {}
	for _, p in providers do
		table.insert(listed, { id = p.id, name = p.name, icon = p.icon })
	end
	shared.now:set({ providers = listed })
end

local function settled(value: any, word: string?): any
	local p = { done = true }
	function p.after(_self: any, ok: ((any) -> ())?, bad: ((any) -> ())?)
		if word then
			if bad then
				bad({ message = word })
			end
		elseif ok then
			ok(value)
		end
	end
	return p
end

local function scope_key(scope: any): string
	return if scope.kind == "provider" then scope.provider elseif scope.kind == "project" then scope.spaceId else "all"
end

local function find_file(device: string, p: any): (number?, any?)
	for i, f in files[device] or {} do
		if f.row.hook == p.hook and f.row.name == p.name and scope_key(f.row["for"]) == scope_key(p["for"]) then
			return i, f
		end
	end
	return nil, nil
end

local function connect(id: string)
	accounts[id] = { name = "ada" }
	hosts[id] = hosts[id] or SERVER
end

local engine: { [string]: (p: any, device: string) -> any } = {
	ListSandboxProviders = function(_p, device)
		local out = {}
		for _, p in providers do
			local one = table.clone(p)
			-- (Only the host device keeps the key, so only it says who.)
			one.account = if hosts[p.id] == device then accounts[p.id] else nil
			table.insert(out, one)
		end
		return settled(out)
	end,
	SandboxHost = function(p)
		local host = hosts[p.provider]
		return settled(if host then { provider = p.provider, hostDeviceId = host, connectedOn = "mac" } else nil)
	end,
	ConnectSandboxProvider = function(p)
		if p.token and p.token ~= "" then
			connect(p.provider)
			return settled({ account = accounts[p.provider], hostDeviceId = hosts[p.provider] })
		end
		if p.provider ~= "docker" then
			return settled(nil, "Boat connects with a pasted key.")
		end
		pending = p.provider
		return settled({ deviceCode = { userCode = "WXYZ-1234", verificationUri = "https://login.docker.com/activate", verificationUriComplete = "https://login.docker.com/activate?user_code=WXYZ-1234", expiresIn = 900 } })
	end,
	DisconnectSandboxProvider = function(p)
		accounts[p.provider] = nil
		hosts[p.provider] = nil
		return settled({ ok = true })
	end,
	SetSandboxHost = function(p)
		if not hosts[p.provider] then
			return settled(nil, `{p.provider} isn't connected.`)
		end
		hosts[p.provider] = p.deviceId
		files[p.deviceId] = files[p.deviceId] or {}
		return settled({ ok = true, hostDeviceId = p.deviceId })
	end,
	ListStepScripts = function(_p, device)
		local out = {}
		for _, f in files[device] or {} do
			table.insert(out, f.row)
		end
		return settled(out)
	end,
	ReadStepScript = function(p, device)
		local _, f = find_file(device, p)
		return if f then settled({ content = f.content }) else settled(nil, `Couldn't read {p.name}`)
	end,
	WriteStepScript = function(p, device)
		last_write:set(`{scope_key(p["for"])}/{p.hook}/{p.name}`)
		last_content:set(p.content or "")
		local _, f = find_file(device, p)
		if f then
			f.content = p.content
		else
			files[device] = files[device] or {}
			table.insert(files[device], script(p["for"], p.hook, p.name, `steps/{p.hook}/{p.name}`, p.content or ""))
		end
		return settled({ ok = true, path = `/home/zeron/steps/{p.hook}/{p.name}` })
	end,
	DeleteStepScript = function(p, device)
		local i = find_file(device, p)
		if i then
			table.remove(files[device], i)
		end
		return settled({ ok = true })
	end,
}

-- `sandbox_settings.ask` / `.change`, as the host answers them.
function M.answer(arg: any): any
	local method = arg and arg.method or ""
	local device = arg and arg.device or ""
	calls:push(`{method}@{device}`)
	local answer = engine[method]
	if not answer then
		return settled(nil, "refused")
	end
	-- (No device: this one, the fixture's MacBook.)
	return answer(arg.params or {}, if device == "" then "mac" else device)
end

ui.command {
	id = "sandbox_settings_stub.connected",
	title = "Docker Sandbox connected, devbox its host device",
	run = function()
		connect("docker")
	end,
}
ui.command {
	id = "sandbox_settings_stub.approve",
	title = "The Docker sign-in approved on its page",
	run = function()
		if pending then
			connect(pending)
			pending = nil
		end
	end,
}

return M
