//! The design guide (#35): the core guide comes with a chat's first design
//! call, topic guides through `design_guide`, and every example snippet in
//! them runs as written.

use serde_json::{Value, json};

use crate::seam::{CHAT, OTHER, Seam};

/// The design tools, as Zeron's MCP server lists them.
const DESIGN_TOOLS: [&str; 3] = ["design_execute", "design_state", "design_guide"];

/// A call that makes one Screen.
const SCREEN: &str = r##"Insert({ type: "frame", name: "Screen", width: 100, height: 100, fill: "#FFFFFF" });"##;

/// The guide's topics, as design_guide lists them.
async fn topics(seam: &Seam) -> Vec<String> {
    let listed = seam.call("design_guide", json!({})).await.json();
    serde_json::from_value(listed["topics"].clone()).expect("a list of topics")
}

#[tokio::test]
async fn the_core_guide_comes_with_a_chats_first_design_call_and_not_again() {
    let seam = Seam::new().await;
    let first = seam.create("Guided", SCREEN).await;
    let guide = first.guide.clone().expect("the first call brings the core guide");
    assert!(guide.len() > 10_000 && guide.contains("Insert("), "{guide}");
    // Beside the result, not in it: the result's text is still its JSON.
    assert!(first.json().get("guide").is_none());

    let path = first.path();
    let again = seam.execute(&path, SCREEN).await;
    assert!(again.guide.is_none(), "{:?}", again.guide);
    assert!(seam.call("design_state", json!({})).await.guide.is_none());

    // Another chat gets it with its own first design call, whichever it is.
    let theirs = seam.call_from(OTHER, "design_state", json!({})).await;
    assert_eq!(theirs.guide.as_deref(), Some(guide.as_str()));
    theirs.json();
    let theirs = seam
        .call_from(OTHER, "design_execute", json!({ "path": path, "snippet": SCREEN }))
        .await;
    assert!(theirs.guide.is_none());
    seam.shutdown().await;
}

#[tokio::test]
async fn design_guide_returns_each_topic_and_lists_them_for_an_unknown_one() {
    let seam = Seam::new().await;
    // With no topic, the core guide (again, after compaction say).
    let core = seam.call("design_guide", json!({})).await;
    let core_text = core.guide.clone().expect("the core guide");
    assert!(core.json()["topics"].is_array());
    assert_eq!(seam.call("design_guide", json!({})).await.guide, Some(core_text.clone()));

    let topics = topics(&seam).await;
    let wanted = [
        "components",
        "design-systems",
        "landing-pages",
        "mobile-apps",
        "web-apps",
        "slides",
        "tables",
        "code",
        "tailwind",
    ];
    assert_eq!(topics, wanted);
    for topic in &topics {
        let got = seam.call("design_guide", json!({ "topic": topic })).await;
        assert_eq!(got.json()["topic"], json!(topic));
        let text = got.guide.unwrap_or_else(|| panic!("no guide for {topic}"));
        assert!(text.starts_with("# ") && text.len() > 2_000 && text != core_text, "{topic}: {text}");
    }

    let unknown = seam.call("design_guide", json!({ "topic": "posters" })).await;
    assert!(unknown.is_error, "{}", unknown.text);
    for topic in wanted {
        assert!(unknown.text.contains(topic), "{}", unknown.text);
    }
    seam.shutdown().await;
}

/// The ```js blocks of a guide, in order.
fn snippets(guide: &str) -> Vec<String> {
    let mut snippets = Vec::new();
    let mut open: Option<String> = None;
    for line in guide.lines() {
        match (&mut open, line.trim_start()) {
            (None, "```js") => open = Some(String::new()),
            (Some(_), "```") => snippets.push(open.take().unwrap()),
            (Some(code), line) if !line.is_empty() || !code.is_empty() => {
                code.push_str(line);
                code.push('\n');
            }
            _ => {}
        }
    }
    assert!(open.is_none(), "a ```js block isn't closed");
    snippets
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_example_snippet_in_the_guide_runs() {
    let seam = Seam::new().await;
    let mut guides = vec![("core".to_owned(), seam.call("design_guide", json!({})).await.guide.unwrap())];
    for topic in topics(&seam).await {
        let guide = seam.call("design_guide", json!({ "topic": topic })).await.guide.unwrap();
        guides.push((topic, guide));
    }
    let mut ran = 0;
    for (topic, guide) in guides {
        // Each guide's examples in order, on a Design of their own, from a
        // chat of their own: a later one may use what an earlier one left
        // in its globals.
        let chat = format!("guide-{topic}");
        let mut path: Option<Value> = None;
        for (i, snippet) in snippets(&guide).into_iter().enumerate() {
            let target = match &path {
                Some(path) => json!({ "path": path, "snippet": snippet }),
                None => json!({ "name": format!("Guide {topic}"), "snippet": snippet }),
            };
            let called = seam.call_from(&chat, "design_execute", target).await;
            assert!(!called.is_error, "{topic} example {}:\n{snippet}\nfailed: {}", i + 1, called.text);
            let result = called.json();
            assert!(result.get("warnings").is_none(), "{topic} example {}:\n{snippet}\nwarned: {result}", i + 1);
            if path.is_none() {
                // A photo in its images folder, as the person would give one.
                let file = std::path::PathBuf::from(result["design"]["path"].as_str().unwrap());
                let photo = image::RgbaImage::from_fn(400, 300, |x, y| image::Rgba([(x / 2) as u8, (y / 2) as u8, 120, 255]));
                photo.save(file.parent().unwrap().join("images/photo.png")).unwrap();
                path = Some(result["design"]["path"].clone());
            }
            ran += 1;
        }
    }
    assert!(ran >= 30, "only {ran} examples");
    seam.shutdown().await;
}

#[tokio::test]
async fn a_chat_that_never_designs_gets_the_tool_names_and_one_instruction_line() {
    let seam = Seam::new().await;
    let instructions = seam.instructions().await;
    let lines: Vec<&str> = instructions.lines().filter(|line| line.contains("design_")).collect();
    assert_eq!(lines.len(), 1, "{instructions}");
    assert!(DESIGN_TOOLS.iter().all(|tool| lines[0].contains(tool)), "{}", lines[0]);

    // Each design tool and each of its arguments says what it is in one
    // line, and all of them together stay small: the guide is what teaches.
    let tools = seam.tool_list().await;
    let mut listed = 0;
    for name in DESIGN_TOOLS {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} is listed"));
        let description = tool["description"].as_str().unwrap();
        assert!(!description.is_empty() && !description.contains('\n'), "{description}");
        for (argument, schema) in tool["inputSchema"]["properties"].as_object().unwrap() {
            let description = schema["description"].as_str().unwrap_or_default();
            assert!(!description.contains('\n') && description.len() <= 300, "{name}.{argument}: {description}");
        }
        listed += tool.to_string().len();
    }
    assert!(listed < 3_000, "the design tools take {listed} bytes");
    seam.shutdown().await;
}

#[tokio::test]
async fn a_topic_asked_for_first_comes_with_the_core_guide() {
    let seam = Seam::new().await;
    let core = seam.call_from(OTHER, "design_guide", json!({})).await.guide.unwrap();
    let slides = seam.call_from(OTHER, "design_guide", json!({ "topic": "slides" })).await.guide.unwrap();

    // This chat hasn't had the core guide: it comes after the topic.
    let both = seam.call_from(CHAT, "design_guide", json!({ "topic": "slides" })).await.guide.unwrap();
    assert_eq!(both, format!("{slides}\n\n{core}"));
    assert!(seam.call("design_state", json!({})).await.guide.is_none());
    seam.shutdown().await;
}
