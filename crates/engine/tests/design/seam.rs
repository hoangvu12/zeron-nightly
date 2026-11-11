//! Seam 1: the design tools as an agent calls them. An engine assembled
//! with the mock harness in a temp folder, and Zeron's MCP server for one
//! of its chats, connected as an agent's is (the agent token's guard), and
//! called with `tools/call` the way an agent's client calls it; tests read what the agent would: the result's text and images, and
//! the files on disk. Fonts come from a fixture folder ([`FONTS`]) in place
//! of Google Fonts, so tests run offline. Managed Designs save at once
//! after each change, so tests read their files as each call left them
//! (`saving.rs` sets an autosave delay of its own).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use zeron_design::fonts::{FontFolder, Fonts};
use zeron_design::icons::IconSource;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_proto::HarnessId;

/// The chat the agent calls from.
pub const CHAT: &str = "designer";

/// A second chat, for Designs shared between chats.
pub const OTHER: &str = "implementer";

/// The fixture standing in for Google Fonts: the Zeron app's own font
/// files (Geist, Geist Mono and Instrument Serif).
pub const FONTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/zgx/fonts");

pub struct Seam {
    pub core: EngineCore,
    tools: Arc<Tools>,
    /// Iconify, played by the fixture icons.
    pub iconify: Arc<FixtureIcons>,
    dir: tempfile::TempDir,
}

/// Icons served from `fixtures/icons/<set>/<name>.svg` in place of
/// Iconify's API, so tests run offline; it notes each icon asked for, and
/// takes `delay` over each as a slow network would.
#[derive(Default)]
pub struct FixtureIcons {
    pub fetched: Mutex<Vec<String>>,
    pub delay: Mutex<std::time::Duration>,
}

impl IconSource for FixtureIcons {
    fn fetch(&self, set: &str, name: &str) -> Result<Option<String>, String> {
        self.fetched.lock().unwrap().push(format!("{set}:{name}"));
        std::thread::sleep(*self.delay.lock().unwrap());
        let file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/design/fixtures/icons")
            .join(set)
            .join(format!("{name}.svg"));
        Ok(std::fs::read_to_string(file).ok())
    }
}

/// One tool call's result, as the agent's client gets it.
pub struct Called {
    pub is_error: bool,
    /// The first text content: the result as JSON, or the error's message.
    pub text: String,
    /// The text contents after the first, as one: the design guide, which
    /// the chat's first design call gets, or the one design_guide returns.
    pub guide: Option<String>,
    pub images: Vec<Image>,
}

impl Called {
    /// The text content as JSON (a successful call's).
    pub fn json(&self) -> Value {
        assert!(!self.is_error, "the call failed: {}", self.text);
        serde_json::from_str(&self.text).expect("a result's text is JSON")
    }

    /// The id the call gave the node named `name`.
    pub fn id(&self, name: &str) -> String {
        self.json()["ids"][name]
            .as_str()
            .unwrap_or_else(|| panic!("no id for {name} in {}", self.text))
            .to_owned()
    }

    /// The `.zdesign` file the call changed.
    pub fn path(&self) -> PathBuf {
        PathBuf::from(self.json()["design"]["path"].as_str().expect("a path"))
    }

    /// A failed call's failure: its error, line and edit id.
    pub fn failure(&self) -> Value {
        assert!(self.is_error, "the call succeeded: {}", self.text);
        serde_json::from_str(&self.text).unwrap_or_else(|_| panic!("a failure's text is JSON: {}", self.text))
    }

    /// The lines the snippet printed.
    pub fn printed(&self) -> Vec<String> {
        match self.json().get("printed") {
            Some(printed) => serde_json::from_value(printed.clone()).expect("printed lines"),
            None => Vec::new(),
        }
    }
}

/// An image content block, decoded.
pub struct Image {
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    rgba: image::RgbaImage,
}

impl Image {
    pub fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
        self.rgba.get_pixel(x, y).0
    }
}

impl Seam {
    pub async fn new() -> Self {
        Self::in_dir(tempfile::tempdir().unwrap())
    }

    /// The engine stopped, and another started on its data folder, as when
    /// Zeron restarts (or crashes: a stop saves no Design that isn't
    /// managed).
    pub async fn restart(self) -> Self {
        let Self { core, tools, dir, .. } = self;
        core.shutdown().await;
        // (Its data folder's lock goes with it.)
        drop((core, tools));
        Self::in_dir(dir)
    }

    fn in_dir(dir: tempfile::TempDir) -> Self {
        let core = EngineCore::assemble(
            &dir.path().join("engine"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
        )
        .unwrap();
        core.designs.use_fonts(Fonts::new(
            Some(dir.path().join("font-cache")),
            Some(Box::new(FontFolder::new(FONTS))),
        ));
        core.designs.autosave_after(std::time::Duration::ZERO);
        let tools = agent_tools(&core, CHAT);
        let iconify = Arc::new(FixtureIcons::default());
        core.designs.icons().set_source(iconify.clone());
        Self {
            core,
            tools,
            iconify,
            dir,
        }
    }

    /// The engine's data folder.
    pub fn data_dir(&self) -> PathBuf {
        self.dir.path().join("engine")
    }

    /// Zeron's MCP tool list, as `tools/list` gives it.
    pub async fn tool_list(&self) -> Vec<Value> {
        let reply = mcp(&self.tools, "tools/list", json!({})).await;
        reply["result"]["tools"].as_array().expect("a tool list").clone()
    }

    /// What `initialize` tells the agent about Zeron's tools.
    pub async fn instructions(&self) -> String {
        let reply = mcp(&self.tools, "initialize", json!({})).await;
        reply["result"]["instructions"].as_str().expect("instructions").to_owned()
    }

    pub async fn call(&self, tool: &str, arguments: Value) -> Called {
        call(&self.tools, tool, arguments).await
    }

    /// A call from another chat's agent, made on first use.
    pub async fn call_from(&self, chat: &str, tool: &str, arguments: Value) -> Called {
        call(&agent_tools(&self.core, chat), tool, arguments).await
    }

    /// An RPC from the app's window (a Design tab's), as JSON.
    pub async fn window(&self, method: &str, params: Value) -> Result<Value, String> {
        zeron_rpc::memory_client(self.core.rpc_service())
            .call(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    /// Saves the Design at `path`, as the person does with Ctrl+S: a
    /// Design that isn't managed changes on disk only then.
    pub async fn save(&self, path: &Path) {
        self.window("SaveDesign", json!({ "path": path })).await.expect("saved");
    }

    /// `design_execute` on a new Design named `name`.
    pub async fn create(&self, name: &str, snippet: &str) -> Called {
        self.call("design_execute", json!({ "name": name, "snippet": snippet }))
            .await
    }

    /// `design_execute` on the Design at `path`.
    pub async fn execute(&self, path: &Path, snippet: &str) -> Called {
        self.call("design_execute", json!({ "path": path, "snippet": snippet }))
            .await
    }

    /// `design_execute` retrying the failed call `edit_id` with `edits`.
    pub async fn retry(&self, edit_id: &Value, edits: Value) -> Called {
        self.call("design_execute", json!({ "editId": edit_id, "edits": edits }))
            .await
    }

    /// The managed designs folder.
    pub fn designs_folder(&self) -> PathBuf {
        self.core.designs.root().to_path_buf()
    }

    pub async fn shutdown(self) {
        self.core.shutdown().await;
    }
}

/// Zeron's MCP server for `chat`'s agent, making the chat if it's new.
fn agent_tools(core: &EngineCore, chat: &str) -> Arc<Tools> {
    if core.workspace.chat(chat).unwrap().is_none() {
        core.workspace
            .create_chat(chat, None, Some(&core.device_id), None, None)
            .unwrap();
    }
    Arc::new(Tools::new(Arc::new(Zeron::with_client(
        zeron_rpc::agent_memory_client(core.rpc_service(), Some(chat.into())),
        Origin {
            chat_id: Some(chat.into()),
            device_id: Some(core.device_id.clone()),
            ..Default::default()
        },
    ))))
}

/// An MCP request, answered as the agent's MCP server answers it: a
/// process of its own (`zeron mcp`), so here a thread of its own, apart from
/// the engine's (the two polled on one test thread are deeper than its
/// stack in a Windows debug build).
async fn mcp(tools: &Arc<Tools>, method: &'static str, params: Value) -> Value {
    let (tools, (tell, answer)) = (tools.clone(), tokio::sync::oneshot::channel());
    std::thread::Builder::new()
        .name("zeron-mcp".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let _ = tell.send(runtime.block_on(zeron_mcp::handle_request(&tools, json!(1), method, params)));
        })
        .unwrap();
    answer.await.expect("the MCP server answered")
}

/// What the agent's client keeps of a call's result. Clients that take a
/// structured copy (Claude Code and Codex) keep it alone, in place of the
/// content, so here a result that has one is read as that copy's JSON and
/// nothing else: a guide or image a client would drop isn't seen either.
async fn call(tools: &Arc<Tools>, tool: &str, arguments: Value) -> Called {
    let reply = mcp(tools, "tools/call", json!({ "name": tool, "arguments": arguments })).await;
    let result = &reply["result"];
    let is_error = result["isError"] == true;
    if let Some(structured) = result.get("structuredContent") {
        return Called {
            is_error,
            text: structured.to_string(),
            guide: None,
            images: Vec::new(),
        };
    }
    let content = result["content"].as_array().expect("content");
    let mut texts = content
        .iter()
        .filter(|c| c["type"] == "text")
        .map(|c| c["text"].as_str().unwrap_or_default().to_owned());
    let text = texts.next().unwrap_or_default();
    let guide = texts.reduce(|guide, more| format!("{guide}\n\n{more}"));
    let images = content
        .iter()
        .filter(|c| c["type"] == "image")
        .map(|c| {
            let png = base64::engine::general_purpose::STANDARD
                .decode(c["data"].as_str().expect("image data"))
                .expect("base64");
            let rgba = image::load_from_memory(&png).expect("a PNG").to_rgba8();
            Image {
                mime_type: c["mimeType"].as_str().unwrap_or_default().to_owned(),
                width: rgba.width(),
                height: rgba.height(),
                rgba,
            }
        })
        .collect();
    Called {
        is_error,
        text,
        guide,
        images,
    }
}
