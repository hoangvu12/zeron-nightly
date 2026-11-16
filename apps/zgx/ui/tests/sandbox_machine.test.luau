-- A chat's sandbox on its composer (spec #37 "When things go wrong"). First
-- what's said (screens/chat/sandbox_machine, pure functions); then, on
-- ui/chat_fixture.luau with the host answered by ui/sandbox_stub.luau, the
-- composer's band (a row of its own, always shown, never in the deck's
-- Alerts): Sandbox deleted with Start fresh, a full disk, asleep, and what
-- they ask of the host.

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

test("deleted outside Zeron: where, and Start fresh, nothing more (screen 14)", function()
	local a = logic.alert(steps({ state = "gone", error = "The sandbox was deleted outside Zeron." })) :: any
	expect(a.kind).to_be("gone")
	expect(a.title).to_be("Sandbox deleted in Docker")
	expect(a.detail).to_be(nil)
	expect(a.action).to_be("fresh")
	expect(a.action_label).to_be("Start fresh")
	local unnamed = logic.alert(steps({ state = "gone", provider_name = false })) :: any
	expect(unnamed.title).to_be("Sandbox deleted")
end)

test("a full disk says so, with Start fresh", function()
	local a = logic.alert(steps({
		state = "failed",
		disk_full = true,
		error = "The sandbox's disk is full. Free some space, or start fresh.\nNo space left on device",
	})) :: any
	expect(a.kind).to_be("disk")
	expect(a.tone).to_be("danger")
	expect(a.title).to_be("Sandbox's disk is full")
	expect(a.detail).to_be("Free some space, or start fresh")
	expect(a.action).to_be("fresh")
end)

test("deleted wins over a full disk", function()
	local a = logic.alert(steps({ state = "gone", disk_full = true })) :: any
	expect(a.kind).to_be("gone")
end)

test("as the band: one row in its provider's glyph, a spinner while on its way, and why it didn't go", function()
	local a = logic.alert(steps({ state = "gone" })) :: any
	local idle = logic.band(a, { icon = "docker" })
	expect(idle.kind).to_be("gone")
	expect(idle.icon).to_be("docker")
	expect(idle.tone).to_be(nil)
	expect(idle.action).to_be("Start fresh")
	expect(idle.spinner).to_be(false)
	expect(idle.dismissable).to_be(nil)
	expect(idle.detail).to_be(nil)
	local busy = logic.band(a, { busy = true })
	expect(busy.action).to_be(nil)
	expect(busy.spinner).to_be(true)
	local failed = logic.band(a, { failure = "offline" })
	expect(failed.action).to_be("Start fresh")
	expect(failed.detail).to_be(logic.failure_words("offline"))
	-- The others in their tone, with the warning glyph.
	local disk = logic.band(logic.alert(steps({ disk_full = true })) :: any)
	expect(disk.tone).to_be("danger")
	expect(disk.icon).to_be("danger-triangle")
end)

test("why Start fresh didn't go, by its word", function()
	expect(string.find(logic.failure_words("offline"), "Couldn't reach", 1, true) ~= nil).to_be(true)
	expect(string.find(logic.failure_words("outdated"), "too old", 1, true) ~= nil).to_be(true)
	expect(logic.failure_words("signed_out")).to_be("Sign in again, then try again.")
	expect(logic.failure_words("failed")).to_be("Couldn't start fresh. Try again.")
	expect(logic.failure_words("failed", "wake")).to_be("Couldn't wake it. Try again.")
	expect(string.find(logic.failure_words("outdated", "wake"), "too old to wake", 1, true) ~= nil).to_be(true)
end)

-- ── Sleep and wake (#43) ───────────────────────────────────────────────────

test("asleep: the moon and Wake on the band, no alert", function()
	local s = steps({ state = "asleep" })
	expect(logic.alert(s)).to_be(nil)
	local line = logic.line(s) :: any
	expect(line.text).to_be("Sandbox asleep")
	expect(line.action).to_be("Wake")
	expect(line.icon).to_be("moon")
	local band = logic.asleep_band(line)
	expect(band.kind).to_be("asleep")
	expect(band.icon).to_be("moon")
	expect(band.title).to_be("Sandbox asleep")
	expect(band.action).to_be("Wake")
	expect(band.tone).to_be(nil)
	-- Awake, going to sleep or waking (said in the transcript): no line.
	expect(logic.line(steps({}))).to_be(nil)
	expect(logic.line(steps({ state = "sleeping", step = "sleep" }))).to_be(nil)
	expect(logic.line(steps({ state = "waking", step = "wake" }))).to_be(nil)
	expect(logic.line(nil)).to_be(nil)
end)

test("asleep, or on its way to or from it, is resting: its host is offline, not out of reach", function()
	expect(logic.resting(steps({ state = "asleep" }))).to_be(true)
	expect(logic.resting(steps({ state = "sleeping" }))).to_be(true)
	expect(logic.resting(steps({ state = "waking" }))).to_be(true)
	expect(logic.resting(steps({}))).to_be(false)
	expect(logic.resting(steps({ state = "failed", step = "wake" }))).to_be(false)
	expect(logic.resting(nil)).to_be(false)
end)

test("waking says how long above the composer; a failed wake is left to its alert", function()
	local Line = require("../screens/chat/sandbox_line")
	local waking = Line.line({ state = "waking", step = "wake", since = 100 }, 103) :: any
	expect(waking.title).to_be("Waking sandbox")
	expect(waking.time).to_be("3s")
	expect(waking.spinner).to_be(true)
	expect(waking.icon).to_be(nil)
	expect(Line.line({ state = "asleep", since = 100 }, 103)).to_be(nil)
	expect(Line.line({ state = "failed", step = "wake", since = 100, error = "No." }, 103)).to_be(nil)
	-- (Any other failed step keeps its Run again.)
	expect((Line.line({ state = "failed", step = "getCode", since = 100 }, 103) :: any).action).to_be("Run again")
end)

test("a wake that failed: its reason, and Wake", function()
	local a = logic.alert(steps({ state = "failed", step = "wake", error = "Couldn't start the sandbox." })) :: any
	expect(a.kind).to_be("wake_failed")
	expect(a.tone).to_be("danger")
	expect(a.title).to_be("Sandbox didn't wake")
	expect(a.detail).to_be("Try waking it again")
	expect(a.action).to_be("wake")
	local band = logic.band(a, { failure = "failed" })
	expect(band.action).to_be("Wake")
	expect(band.detail).to_be("Couldn't wake it. Try again.")
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

-- The band: a row of its own over the composer, never behind the deck's
-- Alerts pill.
local function in_band(what: string): string
	return `#composer-band >> {what}`
end

-- The chat fixture's s2 on the box (not this device), its sandbox as the
-- engine says `case`.
local function on_sandbox(case: string): any
	local app = mount("chat_fixture.luau")
	app:resize(960, 760)
	app:command("go_s2")
	app:frames(30)
	-- (On the fixture's Docker sandbox device, box-sb.)
	app:command("sandbox_stub.steps.ready")
	app:command(`sandbox_stub.{case}`)
	app:wait("#never-shown", { timeout_ms = 300 })
	app:frames(30)
	return app
end

test("opening a chat on a sandbox follows it", function()
	local app = on_sandbox("ready")
	expect(asked(app, "follow:s2") >= 1).to_be(true)
	-- A sandbox that's up raises nothing.
	expect(app:has('role:button "Open Alerts"')).to_be(false)
	expect(app:has("#composer-band")).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("a chat hosted here is never asked about", function()
	local app = mount("chat_fixture.luau")
	app:command("go_s2")
	app:frames(30)
	expect(asked(app, "follow:s2")).to_be(0)
end)

test("a chat on another device that isn't a sandbox is never asked about", function()
	local app = mount("chat_fixture.luau")
	app:command("go_s2")
	app:frames(30)
	app:command("session_on_box")
	app:wait("#never-shown", { timeout_ms = 300 })
	app:frames(30)
	expect(asked(app, "follow:s2")).to_be(0)
	expect(app:error()).to_be(nil)
end)

test("deleted outside Zeron: one row says where, and Start fresh starts one", function()
	local app = on_sandbox("gone")
	expect(app:has(in_band('~"Sandbox deleted in Docker"'))).to_be(true)
	-- Never behind the Alerts pill, and no word more (screen 14).
	expect(app:has('role:button "Open Alerts"')).to_be(false)
	expect(app:has("#composer-band-detail")).to_be(false)
	app:click(in_band('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(1)
	-- The engine starts it: the alert goes.
	app:command("sandbox_stub.started")
	app:frames(30)
	expect(app:has(in_band('~"Sandbox deleted"'))).to_be(false)
	expect(app:error()).to_be(nil)
end)

test("on a provider whose name isn't known, it's just deleted", function()
	local app = on_sandbox("gone_unnamed")
	expect(app:has(in_band('~"Sandbox deleted"'))).to_be(true)
	expect(app:has(in_band('~"Sandbox deleted in"'))).to_be(false)
end)

test("Start fresh on its way: no second one asked", function()
	local app = on_sandbox("gone")
	app:command("sandbox_stub.hold_next")
	app:click(in_band('role:button "Start fresh"'))
	app:frames(30)
	expect(app:has(in_band('role:button "Start fresh"'))).to_be(false)
	expect(asked(app, "fresh:s2")).to_be(1)
end)

test("a Start fresh that didn't go says why, and can be tried again", function()
	local app = on_sandbox("gone")
	app:command("sandbox_stub.fail_next")
	app:click(in_band('role:button "Start fresh"'))
	app:frames(30)
	expect(app:has(in_band('~"Couldn\'t reach Zeron"'))).to_be(true)
	app:click(in_band('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(2)
	expect(app:error()).to_be(nil)
end)

test("asleep: always shown over the composer, and its Wake wakes it", function()
	local app = on_sandbox("asleep")
	expect(app:has(in_band('~"Sandbox asleep"'))).to_be(true)
	expect(app:has('role:button "Open Alerts"')).to_be(false)
	app:click(in_band('role:button "Wake"'))
	app:frames(30)
	expect(asked(app, "wake:s2")).to_be(1)
	-- Waking, the band says so instead.
	app:command("sandbox_stub.waking")
	app:frames(30)
	expect(app:has(in_band('~"Sandbox asleep"'))).to_be(false)
	expect(app:has(in_band('~"Waking sandbox"'))).to_be(true)
	expect(app:error()).to_be(nil)
end)

test("a wake that failed says why, and Wake tries again", function()
	local app = on_sandbox("wake_failed")
	expect(app:has(in_band('~"Sandbox didn\'t wake"'))).to_be(true)
	app:click(in_band('role:button "Wake"'))
	app:frames(30)
	expect(asked(app, "wake:s2")).to_be(1)
	expect(app:error()).to_be(nil)
end)

test("a full disk is said in the chat, with Start fresh", function()
	local app = on_sandbox("disk_full")
	expect(app:has(in_band('~"Sandbox\'s disk is full"'))).to_be(true)
	app:click(in_band('role:button "Start fresh"'))
	app:frames(30)
	expect(asked(app, "fresh:s2")).to_be(1)
end)
