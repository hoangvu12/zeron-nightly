-- Save as sandbox template (spec #37, design 21–23): what a chat's save
-- says on its composer's band while it saves, once it's saved, and when it
-- isn't (screens/shell/save_sandbox_template, pure functions).

local words = require("../screens/shell/save_sandbox_template")

-- (All findable by one filter: `zgx test ui "Save as sandbox template:"`.)
local named_test = test
local function test(name: string, body: () -> ())
	named_test(`Save as sandbox template: {name}`, body)
end

test("it saves under the name trimmed, and a blank one saves nothing", function()
	expect(words.trimmed("  reportly-dev ")).to_be("reportly-dev")
	expect(words.saving(" reportly-dev ").name).to_be("reportly-dev")
	expect(words.can_start(nil, "reportly-dev")).to_be(true)
	expect(words.can_start(nil, "   ")).to_be(false)
end)

test("while it saves: a spinner and its name in mono, no second save, and it stays", function()
	local save = words.saving("reportly-dev")
	local b = words.band(save)
	expect(b.spinner).to_be(true)
	expect(b.title).to_be("Saving as template")
	expect(b.detail).to_be("reportly-dev")
	expect(b.mono).to_be(true)
	expect(b.dismissable).to_be(nil)
	expect(words.can_start(save, "other")).to_be(false)
	expect(words.dismissable(save)).to_be(false)
end)

test("saved (screen 23): a green check, its label, the name in mono, put away with ×", function()
	local save = words.saved(words.saving("reportly-dev"), { id = "tmpl_1", name = "reportly-dev" })
	local b = words.band(save)
	expect(b.icon).to_be("check-circle")
	expect(b.tone).to_be("success")
	expect(b.title).to_be("Saved as template")
	expect(b.detail).to_be("reportly-dev")
	expect(b.mono).to_be(true)
	expect(b.dismissable).to_be(true)
	expect(words.dismissable(save)).to_be(true)
	expect(words.can_start(save, "again")).to_be(true)
	-- (No name back: the one asked for.)
	expect(words.saved(words.saving("x"), nil).name).to_be("x")
	expect(words.band(nil)).to_be(nil)
end)

test("not saved: the engine's words, or ours when there are none", function()
	local save = words.saving("reportly-dev")
	local busy = words.band(words.failed(save, { message = "The chat is still working. Save it as a template once it's done." }))
	expect(busy.tone).to_be("danger")
	expect(busy.dismissable).to_be(true)
	expect(busy.title).to_be("Couldn’t save the template")
	expect(busy.detail).to_be("The chat is still working. Save it as a template once it's done.")
	expect(words.band(words.failed(save, { message = "offline" })).detail).to_be(nil)
	expect(words.band(words.failed(save, "failed")).title).to_be("Couldn’t save the template")
end)
