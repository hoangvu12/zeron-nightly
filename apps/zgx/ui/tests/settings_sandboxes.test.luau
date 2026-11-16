-- Settings › Sandboxes (spec #37, ticket #48). First its words and rows
-- (screens/settings/sandbox_words, pure): When as five moments and Replace a
-- step…, For, one row per script that exists, which host device sleeps.
-- Then the page on ui/shell_fixture.luau, the engine answered by
-- ui/sandbox_settings_stub.luau: provider rows Connect / Disconnect, the
-- host device and its note, the scripts list, Add script and editing one,
-- every script call sent to the host device.

local words = require("../screens/settings/sandbox_words")

-- (All findable by one filter: `zgx test ui "Sandboxes:"`.)
local named_test = test
local function test(name: string, body: () -> ())
	named_test(`Sandboxes: {name}`, body)
end

local PROVIDERS = {
	{ id = "docker", name = "Docker Sandbox", icon = "docker" },
	{ id = "boat", name = "Boat", icon = "boat" },
}
local PROJECTS = {
	{ id = "reportly", name = "reportly", device = "box" },
	{ id = "atlas", name = "Atlas", device = "mac" },
}

-- Words ───────────────────────────────────────────────────────────────────

test("When offers five moments, then Replace a step", function()
	local labels = {}
	for _, o in words.when_options() do
		table.insert(labels, o.label)
	end
	expect(table.concat(labels, "|")).to_be("When it's created|When it's ready|When it wakes|Before it sleeps|Before it's deleted|Replace a step…")
	expect(words.hook_of("after-wake")).to_be("after-wake")
	expect(words.hook_of(words.REPLACE, "get-code")).to_be("instead-of-get-code")
	-- Replace without a step names nothing yet.
	expect(words.hook_of(words.REPLACE, nil)).to_be(nil)
	-- Ready isn't a step to stand in for.
	for _, o in words.step_options() do
		expect(o.value ~= "ready").to_be(true)
	end
end)

test("a script's When reads plainly, whatever hook it's on", function()
	expect(words.when("after-ready")).to_be("When it's ready")
	expect(words.when("before-sleep")).to_be("Before it sleeps")
	expect(words.when("setup")).to_be("When it's ready")
	expect(words.when("instead-of-get-code")).to_be("Instead of Get the code")
	expect(words.when("before-install-zeron")).to_be("Before Install Zeron")
	expect(words.when("after-setup")).to_be("After Setup")
	-- And back to the choices that name it.
	local choice, step = words.choice_of("instead-of-secrets")
	expect(choice).to_be(words.REPLACE)
	expect(step).to_be("secrets")
	expect((words.choice_of("setup"))).to_be("after-ready")
end)

test("one row per script there is: name, When, For", function()
	local rows = words.rows({
		{ ["for"] = { kind = "all" }, hook = "after-ready", name = "fleet-sync.sh" },
		{ ["for"] = { kind = "provider", provider = "docker" }, hook = "after-wake", name = "git-fetch.sh" },
		{ ["for"] = { kind = "project", spaceId = "reportly" }, hook = "setup", name = "setup.sh" },
	}, PROVIDERS, PROJECTS)
	expect(#rows).to_be(3)
	local said = {}
	for _, r in rows do
		table.insert(said, `{r.name}|{r.when}|{r.whom}`)
	end
	expect(said[1]).to_be("fleet-sync.sh|When it's ready|All sandboxes")
	expect(said[2]).to_be("git-fetch.sh|When it wakes|Docker Sandbox")
	expect(said[3]).to_be("setup.sh|When it's ready|reportly")
	-- No empty rows: nothing is nothing.
	expect(#words.rows({}, PROVIDERS, PROJECTS)).to_be(0)
	expect(#words.rows(nil, PROVIDERS, PROJECTS)).to_be(0)
	-- Each row's key is the script's own.
	expect(rows[2].key).to_be("provider:docker/after-wake/git-fetch.sh")
end)

test("For offers every sandbox, each provider and the host device's projects", function()
	local labels = {}
	for _, o in words.for_options(PROVIDERS, PROJECTS, "box") do
		table.insert(labels, o.label)
	end
	expect(table.concat(labels, "|")).to_be("All sandboxes|Docker Sandbox|Boat|reportly")
	expect(words.scope_of("project:reportly").spaceId).to_be("reportly")
	expect(words.scope_of("provider:boat").provider).to_be("boat")
	expect(words.scope_of("all").kind).to_be("all")
	expect(words.scope_value({ kind = "provider", provider = "boat" })).to_be("provider:boat")
end)

test("a script's name is one plain file name", function()
	expect(words.valid_name("warm-cache.sh")).to_be(true)
	for _, bad in { "", ".", "..", ".hidden", "a/b.sh", "a\\b.sh", "c:d" } do
		expect(words.valid_name(bad)).to_be(false)
	end
end)

test("only a host device that isn't always on sleeps", function()
	expect(words.sleeps({ platform = "macos", headless = false })).to_be(true)
	expect(words.sleeps({ platform = "windows" })).to_be(true)
	expect(words.sleeps({ platform = "linux", headless = true })).to_be(false)
	expect(words.sleeps(nil)).to_be(false)
end)

-- The page ────────────────────────────────────────────────────────────────

local PAGE = "#settings-page"
local NAV = "#settings-nav"

local function open(before: { string }?): any
	local app = mount("shell_fixture.luau")
	app:resize(1200, 900)
	app:frames(30)
	app:command("settings_stub.sign_in")
	for _, c in before or {} do
		app:command(c)
	end
	app:command("shell.settings")
	app:frames(20)
	app:click(`{NAV} >> role:tab "Sandboxes"`)
	app:frames(40)
	return app
end

local function shows(app: any, scope: string, text: string): boolean
	return app:tree(scope):find(text, 1, true) ~= nil
end

-- What the page asked of the engine (the stub's log): "method@device".
local function asked(app: any, entry: string): boolean
	for _, c in (app:state("sandbox_settings_stub_calls") :: any) or {} do
		if c == entry then
			return true
		end
	end
	return false
end

test("each provider says whether it's connected, with Connect or Disconnect", function()
	local app = open()
	expect(shows(app, PAGE, "Docker Sandbox")).to_be(true)
	expect(shows(app, PAGE, "Boat")).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Connect Docker Sandbox"`)).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Disconnect Docker Sandbox"`)).to_be(false)
	-- Not connected, nothing names a host device.
	expect(shows(app, PAGE, "Host device")).to_be(false)

	-- Connected with a pasted token: the host device keeps it.
	app:click(`{PAGE} >> role:button "Connect Docker Sandbox"`)
	app:frames(20)
	app:fill('role:dialog >> "Token"', "user:dckr_pat_x")
	app:click('role:dialog >> role:button "Connect"')
	app:frames(40)
	expect(asked(app, "ConnectSandboxProvider@")).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Disconnect Docker Sandbox"`)).to_be(true)
	expect(shows(app, PAGE, "Connected")).to_be(true)
	expect(shows(app, PAGE, "Host device")).to_be(true)
	-- The account is the host device's to say.
	expect(asked(app, "ListSandboxProviders@box")).to_be(true)

	-- Disconnect, after a confirm.
	app:click(`{PAGE} >> role:button "Disconnect Docker Sandbox"`)
	app:frames(20)
	app:click('role:dialog >> role:button "Disconnect"')
	app:frames(40)
	expect(asked(app, "DisconnectSandboxProvider@")).to_be(true)
	expect(app:has(`{PAGE} >> role:button "Connect Docker Sandbox"`)).to_be(true)
end)

test("Docker connects on its own page: the code shows until it's approved", function()
	local app = open()
	app:click(`{PAGE} >> role:button "Connect Docker Sandbox"`)
	app:frames(20)
	app:click('role:dialog >> role:button "Sign in with Docker Sandbox"')
	app:frames(20)
	expect(shows(app, "role:dialog", "WXYZ-1234")).to_be(true)
	app:command("sandbox_settings_stub.approve")
	expect(app:wait(`{PAGE} >> role:button "Disconnect Docker Sandbox"`, { timeout_ms = 15000 })).to_be(true)
end)

test("the host device is picked from those that can be one; a laptop says it sleeps", function()
	local app = open({ "sandbox_settings_stub.connected" })
	expect(shows(app, PAGE, "Host device")).to_be(true)
	-- devbox is an always-on server: no note.
	expect(shows(app, PAGE, "sleeps")).to_be(false)
	app:click(`{PAGE} >> role:button "Host device"`)
	app:frames(20)
	app:click('role:option ~"MacBook Pro"')
	app:frames(40)
	expect(asked(app, "SetSandboxHost@")).to_be(true)
	-- The MacBook isn't always on: the note says so.
	expect(shows(app, PAGE, "sleeps")).to_be(true)
end)

test("the scripts list one row each, from the host device", function()
	local app = open({ "sandbox_settings_stub.connected" })
	expect(asked(app, "ListStepScripts@box")).to_be(true)
	for _, text in { "fleet-sync.sh", "git-fetch.sh", "setup.sh", "When it wakes", "All sandboxes", "Borealis" } do
		expect(shows(app, PAGE, text)).to_be(true)
	end
	-- No descriptions and no "X · Y · Z" strings.
	expect(shows(app, PAGE, " · ")).to_be(false)
end)

test("Add script asks When and For, then saves the file on the host device", function()
	local app = open({ "sandbox_settings_stub.connected" })
	app:click(`{PAGE} >> role:button "Add script"`)
	app:frames(20)
	expect(app:has('role:dialog >> "When"')).to_be(true)
	expect(app:has('role:dialog >> "For"')).to_be(true)
	app:click('role:dialog >> role:button "When"')
	app:frames(10)
	app:click('role:option ~"When it wakes"')
	app:frames(10)
	app:fill('role:dialog >> "File name"', "warm-cache.sh")
	app:fill('role:dialog >> "Script"', "#!/usr/bin/env bash\npnpm install --prefer-offline\n")
	app:click('role:dialog >> role:button "Save"')
	app:frames(40)
	expect(asked(app, "WriteStepScript@box")).to_be(true)
	expect(app:state("sandbox_settings_stub_last_write")).to_be("all/after-wake/warm-cache.sh")
	expect(shows(app, PAGE, "warm-cache.sh")).to_be(true)
end)

test("Replace a step asks which step", function()
	local app = open({ "sandbox_settings_stub.connected" })
	app:click(`{PAGE} >> role:button "Add script"`)
	app:frames(20)
	app:click('role:dialog >> role:button "When"')
	app:frames(10)
	app:click('role:option ~"Replace a step…"')
	app:frames(10)
	app:click('role:dialog >> role:button "Step"')
	app:frames(10)
	app:click('role:option ~"Get the code"')
	app:frames(10)
	app:fill('role:dialog >> "File name"', "clone.sh")
	app:fill('role:dialog >> "Script"', "git clone x\n")
	app:click('role:dialog >> role:button "Save"')
	app:frames(40)
	expect(app:state("sandbox_settings_stub_last_write")).to_be("all/instead-of-get-code/clone.sh")
end)

test("a script opens in the editor and saves to the host device", function()
	local app = open({ "sandbox_settings_stub.connected" })
	app:click(`{PAGE} >> role:button "fleet-sync.sh"`)
	app:frames(30)
	expect(asked(app, "ReadStepScript@box")).to_be(true)
	expect(app:text('role:dialog >> "Script"')).to_contain("fleet")
	app:fill('role:dialog >> "Script"', "echo changed\n")
	app:click('role:dialog >> role:button "Save"')
	app:frames(40)
	expect(asked(app, "WriteStepScript@box")).to_be(true)
	expect(app:state("sandbox_settings_stub_last_content")).to_be("echo changed\n")
end)
