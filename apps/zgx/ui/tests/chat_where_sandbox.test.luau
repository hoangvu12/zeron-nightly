-- Places in a new chat's Work in (spec #37, screens/chat/where): a sandbox's
-- device is never listed, its provider is, once, by its own name and icon;
-- picking it opens its templates (the account's, and Blank); the chip says
-- provider › template › project › branch; a send makes the chat there with
-- the template picked. Then, on the open chat, its sandbox's band (a row of
-- its own over the composer: the step, how long, its segments) and Run
-- again. On ui/chat_fixture.luau: its devices hold a sandbox of
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

test("Sandbox line: starting names the step, how long (apart, muted) and its steps as segments", function()
	local progress = { "done", "done", "running", "pending", "pending", "pending", "pending", "pending" }
	local line = logic.line({ state = "starting", step = "installZeron", since = 1000, progress = progress }, 1012) :: any
	expect(line.title).to_be("Installing Zeron")
	expect(line.time).to_be("12s")
	expect(line.spinner).to_be(true)
	-- Progress isn't a warning: no glyph of its own, no tone.
	expect(line.icon).to_be(nil)
	expect(line.tone).to_be(nil)
	expect(line.action).to_be(nil)
	expect(#line.progress).to_be(8)
	expect(line.progress[3]).to_be("running")
	local later = logic.line({ state = "starting", step = "getCode", since = 1000 }, 1065) :: any
	expect(later.title).to_be("Getting the code")
	expect(later.time).to_be("1m 05s")
	expect((logic.line({ state = "starting", step = "create", since = 1 }, 2) :: any).title).to_be("Creating sandbox")
end)

test("Sandbox line: a failed step says why, with Run again", function()
	local line = logic.line({ state = "failed", step = "getCode", since = 1, error = "Couldn't clone acme/atlas" }, 5) :: any
	expect(line.title).to_be("Getting the code failed")
	expect(line.detail).to_be("Couldn't clone acme/atlas")
	expect(line.tone).to_be("danger")
	expect(line.action).to_be("Run again")
	expect(line.dismissable).to_be(nil)
end)

test("Sandbox line: a script running is named (screen 13)", function()
	local run = { name = "setup.sh", script = "reportly/.zeron/setup.sh", step = "setup", state = "running", since = 3 }
	local line = logic.line({ state = "ready", since = 1, scripts = { run } }, 5) :: any
	expect(line.title).to_be("Running setup.sh")
	expect(line.spinner).to_be(true)
end)

test("Sandbox line: a script that failed, its path in mono, Run again and × (screen 12)", function()
	local run = { name = "setup.sh", script = "reportly/.zeron/setup.sh", step = "setup", state = "failed", since = 3, error = "exit 1" }
	local line = logic.line({ state = "ready", since = 1, scripts = { run } }, 5) :: any
	expect(line.title).to_be("Setup failed")
	expect(line.detail).to_be("reportly/.zeron/setup.sh")
	expect(line.mono).to_be(true)
	expect(line.tone).to_be("danger")
	expect(line.action).to_be("Run again")
	expect(line.dismissable).to_be(true)
	-- Put away with its ×: gone, until it fails again.
	expect(logic.line({ state = "ready", since = 1, scripts = { run } }, 5, { [line.key] = true })).to_be(nil)
	local again = table.clone(run)
	again.since = 9
	expect(logic.line({ state = "ready", since = 1, scripts = { again } }, 10, { [line.key] = true })).never.to_be(nil)
end)

test("Sandbox line: ready, how long it took and whether it was fresh", function()
	expect(logic.ready({ state = "starting", started = { at = 1000 } })).to_be(nil)
	local r = logic.ready({ state = "ready", started = { at = 1000, ready_at = 1052, fresh = false } }) :: any
	expect(r.took).to_be("52s")
	expect(r.fresh).to_be(false)
	expect((logic.ready({ state = "ready", started = { at = 1000, ready_at = 1058, fresh = true } }) :: any).fresh).to_be(true)
end)

test("Sandbox line: the ready row sits after the prompt that waited for it", function()
	local ready = { took = "52s", at = 1052, fresh = false }
	local messages = {
		{ id = "u1", role = "user", text = "Add a landing page", sent = 1000 },
		{ id = "a1", role = "assistant", text = "On it.", started = 1053 },
	}
	local out = logic.with_ready(messages, ready, "Sandbox ready")
	expect(#out).to_be(3)
	expect(out[1].id).to_be("u1")
	expect(out[2].role).to_be("event")
	expect(out[2].text).to_be("Sandbox ready")
	expect(out[2].detail).to_be("52s")
	expect(out[2].icon).to_be("check-circle")
	expect(out[3].id).to_be("a1")
	-- Nothing since (Start fresh): at the end.
	local alone = logic.with_ready({ messages[1] }, ready, "Sandbox ready")
	expect(alone[2].role).to_be("event")
	-- None known: as they are.
	expect(#logic.with_ready(messages, nil, "Sandbox ready")).to_be(2)
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
	expect(chosen(app, "Sandbox template")).to_contain("zeron-dev")
	-- No checkout to choose: its sandbox clones the project.
	panel(app)
	expect(showing(app, 'role:menuitem "Checkout"')).to_be(false)
	-- Picked again, it opens them again.
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	expect(showing(app, 'role:option ~"zgx-dev"')).to_be(true)
end)

test("the provider's row has a chevron; its submenu: ‹ the provider, Templates, ✓ on the one picked (screens 01, 02)", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	expect(app:has('role:option ~"Docker Sandbox" >> .menu-chevron')).to_be(true)
	expect(app:has('role:option ~"MacBook Pro" >> .menu-chevron')).to_be(false)
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	expect(showing(app, "#composer-place-templates-back")).to_be(true)
	expect(app:text("#composer-place-templates-back")).to_contain("Docker Sandbox")
	expect(showing(app, '~"Templates"')).to_be(true)
	expect(app:has('role:option ~"zeron-dev" >> .dropdown-check')).to_be(true)
	expect(app:has('role:option ~"zgx-dev" >> .dropdown-check')).to_be(false)
	-- ‹ goes back to the devices.
	app:click("#composer-place-templates-back")
	rested(app)
	expect(showing(app, 'role:option ~"MacBook Pro"')).to_be(true)
end)

test("a place starts on the template last used there, else the first listed", function()
	local app = mount("chat_fixture.luau")
	home(app)
	app:command("sandbox_stub.offer")
	settle(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	close(app)
	-- None used yet: the first.
	expect(chosen(app, "Sandbox template")).to_contain("zeron-dev")
	panel(app)
	open_row(app, "Sandbox template")
	app:click('role:option ~"zgx-dev"')
	close(app)
	-- Away and back: the one last used.
	open_row(app, "Device")
	app:click('role:option ~"MacBook Pro"')
	close(app)
	open_row(app, "Device")
	app:click('role:option ~"Docker Sandbox"')
	rested(app)
	close(app)
	expect(chosen(app, "Sandbox template")).to_contain("zgx-dev")
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

local BAND = "#composer-band"

test("a sandbox starting says its step, a timer and its segments, always shown, its steps followed", function()
	local app = on_sandbox("starting")
	expect(app:state("calls")).to_contain("follow:s2")
	-- Its own row over the composer, never behind the Alerts pill (05, 15).
	expect(app:has('role:button "Open Alerts"')).to_be(false)
	expect(app:text(`{BAND} >> #composer-band-title[1]`)).to_be("Installing Zeron")
	expect(app:text(`{BAND} >> #composer-band-time[1]`)).to_contain("s")
	expect(app:count(`{BAND} >> .composer-band-segment-done`)).to_be(2)
	expect(app:count(`{BAND} >> .composer-band-segment-running`)).to_be(1)
	expect(app:count(`{BAND} >> .composer-band-segment-pending`)).to_be(5)
	-- Never that its host can't be reached.
	expect(app:has('~"Can\'t reach"')).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("a failed step says why, and Run again runs it", function()
	local app = on_sandbox("failed")
	expect(app:text(`{BAND} >> #composer-band-title[1]`)).to_be("Getting the code failed")
	expect(app:text(`{BAND} >> #composer-band-detail[1]`)).to_be("Couldn't clone acme/atlas")
	app:click(`{BAND} >> role:button "Run again"`)
	rested(app)
	expect(app:state("calls")).to_contain("run_again:s2")
	expect(app:text(`{BAND} >> #composer-band-title[1]`)).to_be("Getting the code")
end)

test("the open chat's chip reads as its provider and template", function()
	local app = on_sandbox("ready")
	expect(app:has('~"Installing Zeron"')).to_be(false)
	expect(app:has('#composer-place-device >> ~"Docker Sandbox"')).to_be(true)
	expect(app:has('#composer-place-template >> ~"zeron-dev"')).to_be(true)
end)

test("a script running, then failed: named, its path in mono, put away with ×", function()
	local app = on_sandbox("ready")
	app:command("sandbox_stub.script_running")
	rested(app)
	expect(app:text(`{BAND} >> #composer-band-title[1]`)).to_be("Running setup.sh")
	app:command("sandbox_stub.script_failed")
	rested(app)
	expect(app:text(`{BAND} >> #composer-band-title[1]`)).to_be("Setup failed")
	expect(app:text(`{BAND} >> #composer-band-detail[1]`)).to_be("reportly/.zeron/setup.sh")
	expect(app:has(`{BAND} >> role:button "Run again"`)).to_be(true)
	app:click(`{BAND} >> role:button ~"Dismiss"`)
	rested(app)
	expect(app:has(BAND)).to_be(false)
end)

test("a fresh sandbox that got ready says so in the transcript, with how long", function()
	local app = on_sandbox("ready")
	app:command("sandbox_stub.fresh_ready")
	rested(app)
	expect(app:has('.message-event >> ~"Fresh sandbox ready"')).to_be(true)
	expect(app:has('.message-event >> ~"58s"')).to_be(true)
	expect(app:error()).to_be(nil)
end)

test("a chat on a sandbox is Sandboxed, the shield in the access chip's place (screen 03)", function()
	local app = on_sandbox("ready")
	expect(app:has('role:button "Access" >> ~"Sandboxed"')).to_be(true)
	expect(app:has('role:button "Access" >> ~"Full access"')).to_be(false)
	-- A new chat on a place too; elsewhere, the access modes.
	local new = mount("chat_fixture.luau")
	home(new)
	expect(new:has('role:button "Access" >> ~"Sandboxed"')).to_be(false)
	new:command("sandbox_stub.offer")
	settle(new)
	open_row(new, "Device")
	new:click('role:option ~"Docker Sandbox"')
	rested(new)
	close(new)
	expect(new:has('role:button "Access" >> ~"Sandboxed"')).to_be(true)
end)

test("the agent menu: Agents over Model, one click to switch, the chip reading the agent (screens 04, 17, 19)", function()
	local app = on_sandbox("ready")
	app:command("catalog_grows")
	app:set_state("chat_model", "opus")
	rested(app)
	app:click('role:button "Model"')
	rested(app)
	expect(showing(app, '~"Agents"')).to_be(true)
	expect(showing(app, "#agent-menu-claude-code")).to_be(true)
	expect(showing(app, "#agent-menu-codex")).to_be(true)
	-- The running agent's models alone, under Model, ✓ on the one picked.
	expect(showing(app, 'role:option ~"Opus"')).to_be(true)
	expect(app:has('role:option ~"GPT-5.5"')).to_be(false)
	expect(app:has('role:option ~"Opus" >> .dropdown-check')).to_be(true)
	app:click("#agent-menu-codex")
	rested(app)
	expect(app:state("chat_model")).to_be("gpt")
	close(app)
	rested(app)
	expect(app:state("chat_agent_pick")).to_be("codex")
	-- (Its name rolls in, a letter at a time: wait for it.)
	app:wait('role:button "Model" >> ~"Codex"', { timeout_ms = 2000 })
	expect(app:has('role:button "Model" >> ~"Codex"')).to_be(true)
	expect(app:has('role:button "Model" >> ~"Medium"')).to_be(true)
	-- A model picked by name: the chip reads it.
	app:click('role:button "Model"')
	rested(app)
	app:click('role:option ~"GPT-5.5"')
	app:wait('role:button "Model" >> ~"GPT-5.5"', { timeout_ms = 2000 })
	expect(app:has('role:button "Model" >> ~"GPT-5.5"')).to_be(true)
	expect(app:error()).to_be(nil)
end)
