//! Seam 1: the design tools as an agent calls them. An engine assembled
//! with the mock harness in a temp folder, and Zeron's MCP server for one
//! of its chats, connected as an agent's is (the agent token's guard), and
//! called with `tools/call` the way an agent's client calls it; tests read what the agent would: the result's text and images, and
//! the files on disk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{Value, json};
use zeron_design::icons::IconSource;
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_proto::HarnessId;

/// The chat the agent calls from.
pub const CHAT: &str = "designer";

/// A second chat, for Designs shared between chats.
pub const OTHER: &str = "implementer";

pub struct Seam {
    pub core: EngineCore,
    tools: Tools,
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
    /// The text content: the result as JSON, or the error's message.
    pub text: String,
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
        let dir = tempfile::tempdir().unwrap();
        let core = EngineCore::assemble(
            &dir.path().join("engine"),
            Arc::new(HarnessRegistry::new()),
            HarnessId::Mock,
            None,
        )
        .unwrap();
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
        let reply = zeron_mcp::handle_request(&self.tools, json!(1), "tools/list", json!({})).await;
        reply["result"]["tools"].as_array().expect("a tool list").clone()
    }

    pub async fn call(&self, tool: &str, arguments: Value) -> Called {
        call(&self.tools, tool, arguments).await
    }

    /// A call from another chat's agent, made on first use.
    pub async fn call_from(&self, chat: &str, tool: &str, arguments: Value) -> Called {
        call(&agent_tools(&self.core, chat), tool, arguments).await
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

    /// The managed designs folder.
    pub fn designs_folder(&self) -> PathBuf {
        self.core.designs.root().to_path_buf()
    }

    pub async fn shutdown(self) {
        self.core.shutdown().await;
    }
}

/// Zeron's MCP server for `chat`'s agent, making the chat if it's new.
fn agent_tools(core: &EngineCore, chat: &str) -> Tools {
    if core.workspace.chat(chat).unwrap().is_none() {
        core.workspace
            .create_chat(chat, None, Some(&core.device_id), None, None)
            .unwrap();
    }
    Tools::new(Arc::new(Zeron::with_client(
        zeron_rpc::agent_memory_client(core.rpc_service(), Some(chat.into())),
        Origin {
            chat_id: Some(chat.into()),
            device_id: Some(core.device_id.clone()),
            ..Default::default()
        },
    )))
}

async fn call(tools: &Tools, tool: &str, arguments: Value) -> Called {
    let reply = zeron_mcp::handle_request(
        tools,
        json!(1),
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    )
    .await;
    let result = &reply["result"];
    let content = result["content"].as_array().expect("content");
    let text = content
        .iter()
        .filter(|c| c["type"] == "text")
        .map(|c| c["text"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
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
        is_error: result["isError"] == true,
        text,
        images,
    }
}
