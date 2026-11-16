-- Places in a new chat's Work in (spec #37, screens/chat/where): a sandbox's
-- device is never listed, its provider is, once, by its own name and icon;
-- picking it opens its templates (the account's, and Blank); the chip says
-- provider › template › project › branch; a send makes the chat there with
-- the template picked. Then, on the open chat, its sandbox's progress line
-- and Run again. On ui/chat_fixture.luau: its devices hold a sandbox of
-- Docker's (`box-sb`, as the host reads its capabilities), and
-- `sandbox_stub.*` answers for the host.

local logic = require("../screens/chat/sandbox_line")

local function settle(app: any)
	app:frames(30)
end

local function rested(app: any)
	app:wait("#never-shown", { timeout_ms = 900 })
	settle(app)
end

local function showing(app: any, sel: string): boolean
	if not app:has(sel) then
		return false
	end
	local first = string.match(app:tree(sel), "[^\n]+") or ""
	return string.find(first, " hidden", 1, true) == nil
end

local function home(app: any)
	app:resize(960, 760)
	app:command("go_home")
	settle(app)
end

local function panel(app: any)
	if not showing(app, 'role:menuitem "Device"') then
		app:click('role:button "Branch"')
		settle(app)
	end
end

local function open_row(app: any, row: string)
	panel(app)
	app:click(`role:menuitem "{row}"`)
	settle(app)
end

local function close(app: any)
	app:key("Escape")
	app:key("Escape")
	settle(app)
end

local function chosen(app: any, row: string): string
	rested(app)
	local words = {}
	for line in app:tree(`role:menuitem "{row}"`):gmatch("[^\n]+") do
		local t = line:match('text "([^"]*)"')
		if t then
			table.insert(words, t)
		end
	end
	return table.concat(words, " ")
end

-- ── What the line says (pure) ─────────────────────────────────────────────

test("Sandbox line: starting names the step and how long it's taken", function()
	local line = logic.line({ state = "starting", step = "installZeron", since = 1000 }, 1012) :: any
	expect(line.text).to_be("Installing Zeron · 12s")
	expect(line.spinner).to_be(true)
	expect(line.action).to_be(nil)
	expect((logic.line({ state = "starting", step = "getCode", since = 1000 }, 1065) :: any).text).to_be("Getting the code · 1m 05s")
end)

test("Sandbox line: a failed step says why, with Run again", function()
	local line = logic.line({ state = "failed", step = "getCode", since = 1, error = "Couldn't clone acme/atlas" }, 5) :: any
	expect(line.text).to_be("Getting the code failed: Couldn't clone acme/atlas")
	expect(line.tone).to_be("danger")
	expect(line.action).to_be("Run again")
end)

test("Sandbox line: ready, or not on a place, says nothing", function()
	expect(logic.line({ state = "ready", since = 1 }, 5)).to_be(nil)
	expect(logic.line(nil, 5)).to_be(nil)
end)

-- ── The device picker ─────────────────────────────────────────────────────

test("a sandbox's device is never listed; its provider is, once it's offered", function()
	local app = mount("chat_fixture.luau")
	home(app)
	open_row(app, "Device")
	expect(app:has('role:option ~"MacBook Pro"')).to_be(true)
	expect(app:has('role:option ~"zeron-sandbox"')).to_be(false)
	expect(app:has('role:option ~"Docker Sandbox"')).to_be(false)
	close(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	expect(app:has('role:option ~"Docker Sandbox"')).to_be(true)
	expect(app:has('role:option ~"zeron-sandbox"')).to_be(false)
end)

test("picking the provider opens its templates, the account's and Blank", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	expect(app:state("calls")).to_contain("templates:docker")
	expect(showing(app, 'role:option ~"zeron-dev"')).to_be(true)
	expect(showing(app, 'role:option ~"zgx-dev"')).to_be(true)
	expect(showing(app, 'role:option ~"Blank"')).to_be(true)
	app:click('role:option ~"zeron-dev"')
	settle(app)
	expect(chosen(app, "Template")).to_contain("zeron-dev")
	-- No checkout to choose: its sandbox clones the project.
	panel(app)
	expect(showing(app, 'role:menuitem "Checkout"')).to_be(false)
	-- Picked again, it opens them again.
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	expect(showing(app, 'role:option ~"zgx-dev"')).to_be(true)
end)

test("the chip says provider › template › project › branch, led by the provider's glyph", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	app:click('role:option ~"zeron-dev"')
	close(app)
	rested(app)
	expect(app:text('role:button "Branch" >> #composer-place-device[1]')).to_be("Docker Sandbox")
	expect(app:text('role:button "Branch" >> #composer-place-template[1]')).to_be("zeron-dev")
	expect(app:text('role:button "Branch" >> #composer-place-project[1]')).to_be("Atlas")
	expect(app:has('role:button "Branch" >> .place-lead-docker')).to_be(true)
end)

test("the agents offered are the place's, by its template (as #46 asks them)", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	expect(app:state("calls")).to_contain("catalog_on:place:docker")
	app:click('role:option ~"zeron-dev"')
	rested(app)
	expect(app:state("calls")).to_contain("catalog_on:place:docker:zeron-dev")
end)

test("a place's catalog without the model picked never swaps it (#46 says why on send)", function()
	local app = mount("chat_fixture.luau")
	home(app)
	expect(app:has('role:button "Model" >> ~"Opus"')).to_be(true)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	app:click('role:option ~"zeron-dev"')
	close(app)
	app:command("catalog_place_without_opus")
	rested(app)
	expect(app:has('role:button "Model" >> ~"Opus"')).to_be(true)
	expect(app:error()).to_be(nil)
end)

test("a send on a place makes the chat there from the template picked", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	app:click('role:option ~"zeron-dev"')
	close(app)
	app:type('role:textbox "Message"', "Add a landing page")
	app:key("Enter")
	rested(app)
	local calls = app:state("calls")
	expect(calls).to_contain("|place:docker:zeron-dev")
	expect(calls).to_contain("made:atlas|")
	expect(calls).to_contain("run:/home/agent/workspace/atlas|local|")
end)

test("Blank makes the chat with no template", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	app:click('role:option ~"Blank"')
	close(app)
	app:type('role:textbox "Message"', "Hello")
	app:key("Enter")
	rested(app)
	expect(app:state("calls")).to_contain("|place:docker:blank")
end)

-- ── The open chat ─────────────────────────────────────────────────────────

local function on_sandbox(case: string): any
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	app:command("go_s2")
	settle(app)
	app:command("sandbox_stub.offer")
	app:command(`sandbox_stub.steps.{case}`)
	rested(app)
	return app
end

local function open_deck(app: any)
	if app:has('role:button "Open Alerts"') then
		app:click('role:button "Open Alerts"')
		app:wait("#never-shown", { timeout_ms = 320 })
		app:frames(10)
	end
end

test("a sandbox starting says its step and a timer, its steps followed", function()
	local app = on_sandbox("starting")
	expect(app:state("calls")).to_contain("follow:s2")
	open_deck(app)
	expect(app:has('~"Installing Zeron · "')).to_be(true)
	-- Never that its host can't be reached.
	expect(app:has('~"Can\'t reach"')).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("a failed step says why, and Run again runs it", function()
	local app = on_sandbox("failed")
	open_deck(app)
	expect(app:has('~"Getting the code failed: Couldn\'t clone acme/atlas"')).to_be(true)
	app:click('role:button "Run again"')
	rested(app)
	expect(app:state("calls")).to_contain("run_again:s2")
	expect(app:has('~"Getting the code · "')).to_be(true)
end)

test("the open chat's chip reads as its provider and template", function()
	local app = on_sandbox("ready")
	expect(app:has('~"Installing Zeron"')).to_be(false)
	expect(app:has('#composer-place-device >> ~"Docker Sandbox"')).to_be(true)
	expect(app:has('#composer-place-template >> ~"zeron-dev"')).to_be(true)
end)
