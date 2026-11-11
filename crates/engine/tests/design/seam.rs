//! Seam 1: the design tools as an agent calls them. An engine assembled
//! with the mock harness in a temp folder, and Zeron's MCP server for one
//! of its chats, called with `tools/call` the way an agent's client calls
//! it; tests read what the agent would: the result's text and images, and
//! the files on disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Value, json};
use zeron_engine::{EngineCore, HarnessRegistry};
use zeron_mcp::{Origin, Tools, Zeron};
use zeron_proto::HarnessId;

/// The chat the agent calls from.
pub const CHAT: &str = "designer";

pub struct Seam {
    pub core: EngineCore,
    tools: Tools,
    _dir: tempfile::TempDir,
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
        core.workspace
            .create_chat(CHAT, None, Some(&core.device_id), None, None)
            .unwrap();
        let tools = Tools::new(Arc::new(Zeron::with_client(
            zeron_rpc::memory_client(core.rpc_service()),
            Origin {
                chat_id: Some(CHAT.into()),
                device_id: Some(core.device_id.clone()),
                ..Default::default()
            },
        )));
        Self {
            core,
            tools,
            _dir: dir,
        }
    }

    /// Zeron's MCP tool list, as `tools/list` gives it.
    pub async fn tool_list(&self) -> Vec<Value> {
        let reply = zeron_mcp::handle_request(&self.tools, json!(1), "tools/list", json!({})).await;
        reply["result"]["tools"].as_array().expect("a tool list").clone()
    }

    pub async fn call(&self, tool: &str, arguments: Value) -> Called {
        let reply = zeron_mcp::handle_request(
            &self.tools,
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
