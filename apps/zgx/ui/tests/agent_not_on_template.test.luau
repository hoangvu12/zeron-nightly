-- A chat on a sandbox whose last send was refused: the agent picked isn't on
-- its template (#46, spec #37 stories 51 and 52). The composer says so
-- ("Codex isn't on this template"), has the message's words back, and keeps
-- the agent as picked; the person switches agent and sends, and it goes as
-- the same message. On ui/chat_fixture.luau (`refused_codex`).

local function settle(app: any)
	app:frames(30)
end

local LIST = 'role:list "Alerts"'

local function refused(app: any)
	app:command("go_s2")
	settle(app)
	-- (Codex and its model in the catalog, as the chat ran with it.)
	app:command("catalog_grows")
	settle(app)
	app:command("refused_codex")
	settle(app)
end

local function open_deck(app: any)
	app:click('role:button "Open Alerts"')
	app:wait("#never-shown", { timeout_ms = 320 })
	app:frames(10)
end

test("Agent not on template: the composer says so and has the words back", function()
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	refused(app)
	expect(app:text("role:textbox")).to_be("Add a landing page for reportly")
	open_deck(app)
	expect(app:has(`{LIST} >> ~"Codex isn't on this template"`)).to_be(true)
	-- Never swapped: the agent stays the one picked, until the person picks.
	expect(app:state("chat_model")).to_be("gpt")
	expect(app:state("calls")).never.to_contain("send:")
	expect(app:error()).to_be(nil)
end)

test("Agent not on template: another agent picked, it goes again as the same message", function()
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	refused(app)
	app:set_state("chat_model", "sonnet")
	settle(app)
	app:key("Enter")
	settle(app)
	local calls = app:state("calls")
	expect(calls).to_contain("send:s2:Add a landing page for reportly")
	expect(calls).to_contain("took:s2:claude-code|sonnet|")
	expect(app:state("sent_ids")).to_be("m9,")
	-- Taken: nothing refused says so any more.
	expect(app:has('~"Codex isn\'t on this template"')).to_be(false)
end)

test("Agent not on template: a draft typed already isn't overwritten", function()
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	app:command("go_s2")
	settle(app)
	app:type("role:textbox", "My own draft")
	settle(app)
	app:command("catalog_grows")
	app:command("refused_codex")
	settle(app)
	expect(app:text("role:textbox")).to_be("My own draft")
	open_deck(app)
	expect(app:has(`{LIST} >> ~"Codex isn't on this template"`)).to_be(true)
end)
