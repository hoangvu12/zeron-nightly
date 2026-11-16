-- Save as template (spec #37, design 21–23): which chats offer it, what's
-- asked of the host, and what the chat's composer says while it saves, once
-- it's saved, and when it isn't. The host's calls are answered here
-- (account/sandbox_templates' `ask_*` fields).

local templates = require("../account/sandbox_templates")

-- (All findable by one filter: `zgx test ui "Save as template:"`.)
local named_test = test
local function test(name: string, body: () -> ())
	named_test(`Save as template: {name}`, body)
end

-- A promise the test settles.
local function pending(): any
	local p: any = {}
	function p.after(_self: any, ok: (any) -> (), err: (any) -> ())
		p.ok, p.err = ok, err
	end
	return p
end

local function fresh()
	templates.saves:set({})
	templates.read = function(): { [string]: any }
		return { ["chat-a"] = { provider = "docker" } }
	end
end

test("only a chat on a sandbox kept here offers it", function()
	fresh()
	expect(templates.can_save("chat-a")).to_be(true)
	expect(templates.can_save("chat-b")).to_be(false)
	expect(templates.can_save(nil)).to_be(false)
end)

test("the chats listed are asked about, none when there are none", function()
	local asked: { any } = {}
	templates.ask_check = function(chats: { string })
		table.insert(asked, chats)
	end
	templates.check({})
	templates.check({ "chat-a", "chat-b" })
	expect(#asked).to_be(1)
	expect(asked[1][2]).to_be("chat-b")
end)

test("it saves under the name trimmed, saying so until it's saved", function()
	fresh()
	local asked: any = nil
	local p = pending()
	templates.ask_save = function(chat: string, name: string): any
		asked = { chat = chat, name = name }
		return p
	end
	templates.save("chat-a", "  reportly-dev ")
	expect(asked.chat).to_be("chat-a")
	expect(asked.name).to_be("reportly-dev")
	expect(templates.saving("chat-a")).to_be(true)
	local saving = templates.notice("chat-a")
	expect(saving.spinner).to_be(true)
	expect(string.find(saving.text, "reportly-dev", 1, true) ~= nil).to_be(true)
	-- Saving already: a second Save asks nothing more.
	asked = nil
	templates.save("chat-a", "other")
	expect(asked).to_be(nil)
	-- (Still saving: it can't be put away.)
	templates.dismiss("chat-a")
	expect(templates.saving("chat-a")).to_be(true)

	p.ok({ id = "tmpl_1", name = "reportly-dev" })
	local saved = templates.notice("chat-a")
	expect(saved.tone).to_be("success")
	expect(saved.dismissable).to_be(true)
	expect(saved.text).to_be("Saved as template reportly-dev")
	expect(templates.notice("chat-b")).to_be(nil)
	templates.dismiss("chat-a")
	expect(templates.notice("chat-a")).to_be(nil)
end)

test("a blank name saves nothing", function()
	fresh()
	local asked = false
	templates.ask_save = function(): any
		asked = true
		return pending()
	end
	templates.save("chat-a", "   ")
	expect(asked).to_be(false)
	expect(templates.notice("chat-a")).to_be(nil)
end)

test("not saved: the engine's words, or its own when there are none", function()
	fresh()
	local p = pending()
	templates.ask_save = function(): any
		return p
	end
	templates.save("chat-a", "reportly-dev")
	p.err({ message = "The chat is still working. Save it as a template once it's done." })
	local busy = templates.notice("chat-a")
	expect(busy.tone).to_be("danger")
	expect(busy.text).to_be("The chat is still working. Save it as a template once it's done.")
	-- Again, offline.
	local q = pending()
	templates.ask_save = function(): any
		return q
	end
	templates.save("chat-a", "reportly-dev")
	q.err({ message = "offline" })
	expect(templates.notice("chat-a").text).to_be("Couldn’t save the template")
end)
