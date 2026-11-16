-- A chat's sandbox on its composer (spec #37 "When things go wrong"). First
-- what's said (screens/chat/sandbox_machine, pure functions); then, on
-- ui/chat_fixture.luau with the host answered by ui/sandbox_stub.luau, the
-- composer's deck: Sandbox deleted with Start fresh, a full disk, and what
-- Start fresh asks of the host.

local logic = require("../screens/chat/sandbox_machine")

-- (All findable by one filter: `zgx test ui "Sandbox machine:"`.)
local named_test = test
local function test(name: string, body: () -> ())
	named_test(`Sandbox machine: {name}`, body)
end

-- (`false` for a field it doesn't have: a nil in a table isn't there.)
local function steps(fields: { [string]: any }): any
	local s: { [string]: any } = { state = "ready", provider = "docker", provider_name = "Docker Sandbox", disk_full = false }
	for k, v in fields do
		s[k] = if v == false and k ~= "disk_full" then nil else v
	end
	return s
end

-- ── What's said ────────────────────────────────────────────────────────────

test("a sandbox that's fine, or none, says nothing", function()
	expect(logic.alert(nil)).to_be(nil)
	expect(logic.alert(steps({}))).to_be(nil)
	expect(logic.alert(steps({ state = "starting", step = "create" }))).to_be(nil)
	-- A failure that isn't the disk is the steps line's to say (#41).
	expect(logic.alert(steps({ state = "failed", step = "getCode", error = "auth failed" }))).to_be(nil)
end)

test("the provider is named as a place", function()
	expect(logic.place_name(steps({}))).to_be("Docker")
	expect(logic.place_name(steps({ provider_name = "Modal" }))).to_be("Modal")
	expect(logic.place_name(steps({ provider_name = "sandbox" }))).to_be("sandbox")
	expect(logic.place_name(steps({ provider_name = false }))).to_be(nil)
	expect(logic.place_name(steps({ provider_name = "" }))).to_be(nil)
end)

test("deleted outside Zeron: where, that the conversation's kept, and Start fresh", function()
	local a = logic.alert(steps({ state = "gone", error = "The sandbox was deleted outside Zeron." })) :: any
	expect(a.kind).to_be("gone")
	expect(a.tone).to_be("warning")
	expect(a.title).to_be("Sandbox deleted in Docker")
	expect(a.summary).to_be("The conversation is kept")
	expect(string.find(a.body, "what was pushed comes back", 1, true) ~= nil).to_be(true)
	expect(a.action).to_be("fresh")
	expect(a.action_label).to_be("Start fresh")
	local unnamed = logic.alert(steps({ state = "gone", provider_name = false })) :: any
	expect(unnamed.title).to_be("Sandbox deleted")
end)

test("a full disk says so, in the engine's words, with Start fresh", function()
	local a = logic.alert(steps({
		state = "failed",
		disk_full = true,
		error = "The sandbox's disk is full. Free some space, or start fresh.\nNo space left on device",
	})) :: any
	expect(a.kind).to_be("disk")
	expect(a.tone).to_be("danger")
	expect(a.title).to_be("Sandbox's disk is full")
	expect(a.summary).to_be("Free some space, or start fresh")
	expect(string.find(a.body, "No space left on device", 1, true) ~= nil).to_be(true)
	expect(a.action).to_be("fresh")
	-- With no words of the engine's, its own.
	local plain = logic.alert(steps({ disk_full = true })) :: any
	expect(string.find(plain.body, "ran out of disk", 1, true) ~= nil).to_be(true)
end)

test("deleted wins over a full disk", function()
	local a = logic.alert(steps({ state = "gone", disk_full = true })) :: any
	expect(a.kind).to_be("gone")
end)

test("on the deck: its action, a spinner while on its way, and why it didn't go", function()
	local a = logic.alert(steps({ state = "gone" })) :: any
	local idle = logic.deck_alert(a)
	expect(idle.id).to_be(logic.ALERT_ID)
	expect(idle.action).to_be("Start fresh")
	expect(idle.spinner).to_be(false)
	expect(idle.dismissable).to_be(false)
	expect(idle.note).to_be(nil)
	expect(idle.detail).to_be("The conversation is kept")
	local busy = logic.deck_alert(a, { busy = true })
	expect(busy.action).to_be(nil)
	expect(busy.spinner).to_be(true)
	local failed = logic.deck_alert(a, { failure = "offline" })
	expect(failed.action).to_be("Start fresh")
	expect(failed.note).to_be(logic.failure_words("offline"))
end)

test("why Start fresh didn't go, by its word", function()
	expect(string.find(logic.failure_words("offline"), "Couldn't reach", 1, true) ~= nil).to_be(true)
	expect(string.find(logic.failure_words("outdated"), "too old", 1, true) ~= nil).to_be(true)
	expect(logic.failure_words("signed_out")).to_be("Sign in again, then try again.")
	expect(logic.failure_words("failed")).to_be("Couldn't start fresh. Try again.")
end)

-- ── On the composer ────────────────────────────────────────────────────────

local function calls(app: any): { string }
	return (app:state("sandbox_stub_calls") :: any) or {}
end

local function asked(app: any, entry: string): number
	local n = 0
	for _, c in calls(app) do
		if c == entry then
			n += 1
		end
	end
	return n
end

local LIST = 'role:list "Alerts"'
local function in_deck(what: string): string
	return `{LIST} >> {what}`
end

local function open_deck(app: any)
	app:click('role:button "Open Alerts"')
	app:wait("#never-shown", { timeout_ms = 320 })
	app:frames(10)
end

-- The chat fixture's s2 on the box (not this device), its sandbox as the
-- engine says `case`; the deck opened, if `open`.
local function on_sandbox(case: string, open: boolean?): any
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	app:command("go_s2")
	app:frames(30)
	app:command("session_on_box")
	app:command(`sandbox_stub.{case}`)
	app:wait("#never-shown", { timeout_ms = 300 })
	app:frames(30)
	if open then
		open_deck(app)
	end
	return app
end

test("opening a chat on another device follows its sandbox", function()
	local app = on_sandbox("ready")
	expect(asked(app, "follow:s2") >= 1).to_be(true)
	-- A sandbox that's up raises nothing.
	expect(app:has('role:button "Open Alerts"')).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("a chat hosted here is never asked about", function()
	local app = mount("chat_fixture.luau")
	app:command("go_s2")
	app:frames(30)
	expect(asked(app, "follow:s2")).to_be(0)
end)

test("deleted outside Zeron: the deck says where, and Start fresh starts one", function()
	local app = on_sandbox("gone", true)
	expect(app:has(in_deck('~"Sandbox deleted in Docker"'))).to_be(true)
	app:click(in_deck('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(1)
	-- The engine starts it: the alert goes.
	app:command("sandbox_stub.started")
	app:frames(30)
	expect(app:has(in_deck('~"Sandbox deleted"'))).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("on a provider whose name isn't known, it's just deleted", function()
	local app = on_sandbox("gone_unnamed", true)
	expect(app:has(in_deck('~"Sandbox deleted"'))).to_be(true)
	expect(app:has(in_deck('~"Sandbox deleted in"'))).to_be(false)
end)

test("Start fresh on its way: no second one asked", function()
	local app = on_sandbox("gone", true)
	app:command("sandbox_stub.hold_next")
	app:click(in_deck('role:button "Start fresh"'))
	app:frames(30)
	expect(app:has(in_deck('role:button "Start fresh"'))).to_be(false)
	expect(asked(app, "fresh:s2")).to_be(1)
end)

test("a Start fresh that didn't go says why, and can be tried again", function()
	local app = on_sandbox("gone", true)
	app:command("sandbox_stub.fail_next")
	app:click(in_deck('role:button "Start fresh"'))
	app:frames(30)
	expect(app:has(in_deck('~"Couldn\'t reach Zeron"'))).to_be(true)
	app:click(in_deck('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(2)
	expect(app:error()).to_be(nil)
end)

test("a full disk is said in the chat, with Start fresh", function()
	local app = on_sandbox("disk_full", true)
	expect(app:has(in_deck('~"Sandbox\'s disk is full"'))).to_be(true)
	app:click(in_deck('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(1)
end)
