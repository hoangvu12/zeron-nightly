//! Image fills (#24): images from files beside the Design (or in its
//! `images/` folder) and from http(s) URLs, downloaded once and cached,
//! drawn in cover, contain and stretch modes; a placeholder and a warning
//! for one that can't be had; and only the reference saved. URLs are served
//! by a local server, so nothing reaches the network.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::seam::{Called, Image, Seam};

const WHITE: [u8; 4] = [255, 255, 255, 255];
const BLACK: [u8; 4] = [0, 0, 0, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
/// What an image that can't be had is drawn as.
const PLACEHOLDER: [u8; 4] = [228, 228, 231, 255];

/// A 40 × 20 PNG of four upright stripes: red, green, blue and black.
fn stripes() -> Vec<u8> {
    let image = image::RgbaImage::from_fn(40, 20, |x, _| image::Rgba([RED, GREEN, BLUE, BLACK][x as usize / 10]));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// Serves files by path over HTTP on 127.0.0.1, counting each path's
/// requests; any other path is a 404.
struct Server {
    port: u16,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Server {
    fn start(files: Vec<(&str, Vec<u8>)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let files: HashMap<String, Vec<u8>> = files.into_iter().map(|(path, body)| (path.to_owned(), body)).collect();
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                // The rest of the head.
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
                *counted.lock().unwrap().entry(path.clone()).or_insert(0) += 1;
                let (status, body) = match files.get(&path) {
                    Some(body) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"not here".to_vec()),
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { port, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }
}

/// Makes a Design named `name`, so it has a folder to put images in.
async fn design(seam: &Seam, name: &str) -> std::path::PathBuf {
    seam.create(name, r#"Insert({ type: "frame", name: "Start", width: 1, height: 1 });"#)
        .await
        .path()
}

/// Puts a 300 × 100 white Screen in the Design at `path` with three
/// 100 × 100 rectangles filled with the image at `url`, in cover, contain
/// and stretch modes, and screenshots it.
async fn three_modes(seam: &Seam, path: &Path, url: &str) -> (Called, Image) {
    let snippet = format!(
        r##"const screen = Insert({{ type: "frame", name: "Modes", width: 300, height: 100, fill: "#FFFFFF" }});
const image = (mode) => ({{ type: "image", url: {url}, mode }});
Insert(screen, {{ type: "rectangle", name: "Cover", x: 0, width: 100, height: 100, fill: image("cover") }});
Insert(screen, {{ type: "rectangle", name: "Contain", x: 100, width: 100, height: 100, fill: image("contain") }});
Insert(screen, {{ type: "rectangle", name: "Stretch", x: 200, width: 100, height: 100, fill: image("stretch") }});
TakeScreenshot(screen);"##,
        url = json!(url)
    );
    let mut called = seam.execute(path, &snippet).await;
    assert_eq!(called.images.len(), 1, "{}", called.text);
    let image = called.images.remove(0);
    (called, image)
}

/// The stripes, as each mode lays them over its 100 × 100 rectangle.
#[track_caller]
fn drawn_in_three_modes(image: &Image) {
    // Cover: scaled to 200 × 100 and centered, the outer stripes cut off.
    near(image, (25, 50), GREEN, 2);
    near(image, (75, 50), BLUE, 2);
    // Contain: scaled to 100 × 50 and centered, the rest left empty.
    near(image, (112, 50), RED, 2);
    near(image, (187, 50), BLACK, 2);
    near(image, (150, 10), WHITE, 1);
    near(image, (150, 90), WHITE, 1);
    // Stretch: squeezed to the whole rectangle.
    near(image, (212, 5), RED, 2);
    near(image, (237, 95), GREEN, 2);
    near(image, (262, 50), BLUE, 2);
    near(image, (287, 50), BLACK, 2);
}

#[track_caller]
fn near(image: &Image, (x, y): (u32, u32), expected: [u8; 4], tolerance: u8) {
    let actual = image.rgba(x, y);
    let close = actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= tolerance);
    assert!(close, "at ({x}, {y}): {actual:?}, expected {expected:?} ± {tolerance}");
}

fn warnings(called: &Called) -> Vec<String> {
    let json = called.json();
    let Some(Value::Array(warnings)) = json.get("warnings") else {
        return Vec::new();
    };
    warnings.iter().map(|w| w["message"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn file_images_beside_the_design_draw_in_cover_contain_and_stretch() {
    let seam = Seam::new().await;
    let path = design(&seam, "Photos").await;
    let folder = path.parent().unwrap();
    std::fs::write(folder.join("images").join("stripes.png"), stripes()).unwrap();
    std::fs::write(folder.join("beside.png"), stripes()).unwrap();

    let (called, image) = three_modes(&seam, &path, "images/stripes.png").await;
    assert!(warnings(&called).is_empty(), "{}", called.text);
    drawn_in_three_modes(&image);
    // Beside the file, and with Windows' separator.
    let (_, image) = three_modes(&seam, &path, "beside.png").await;
    drawn_in_three_modes(&image);
    let (_, image) = three_modes(&seam, &path, "images\\stripes.png").await;
    drawn_in_three_modes(&image);
    seam.shutdown().await;
}

#[tokio::test]
async fn url_images_draw_in_every_mode_and_are_downloaded_once() {
    let server = Server::start(vec![("/stripes.png", stripes())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Web").await;
    let url = server.url("/stripes.png");

    let (called, image) = three_modes(&seam, &path, &url).await;
    assert!(warnings(&called).is_empty(), "{}", called.text);
    drawn_in_three_modes(&image);
    // Drawn again, from the cache.
    let screen = called.id("Modes");
    let again = seam.execute(&path, &format!("TakeScreenshot({});", json!(screen))).await;
    assert_eq!(again.images.len(), 1, "{}", again.text);
    drawn_in_three_modes(&again.images[0]);
    assert_eq!(server.hits("/stripes.png"), 1);
    // Nothing downloaded lands beside the Design.
    let beside: Vec<_> = std::fs::read_dir(path.parent().unwrap().join("images")).unwrap().collect();
    assert!(beside.is_empty());
    seam.shutdown().await;
}

#[tokio::test]
async fn an_image_that_cant_be_had_draws_a_placeholder_and_warns() {
    let server = Server::start(vec![("/notes.txt", b"not an image".to_vec())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Broken").await;
    std::fs::write(path.parent().unwrap().join("images").join("notes.png"), "not an image either").unwrap();
    let snippet = format!(
        r##"const screen = Insert({{ type: "frame", name: "Gaps", width: 400, height: 100, fill: "#FFFFFF" }});
const at = (name, x, url) => Insert(screen, {{ type: "rectangle", name, x, width: 100, height: 100,
  fill: {{ type: "image", url }} }});
at("No file", 0, "images/gone.png");
at("No page", 100, {});
at("Not an image", 200, {});
at("Not an image file", 300, "images/notes.png");
TakeScreenshot(screen);"##,
        json!(server.url("/gone.png")),
        json!(server.url("/notes.txt")),
    );
    let called = seam.execute(&path, &snippet).await;
    assert!(!called.is_error, "{}", called.text);
    let warnings = warnings(&called);
    for (name, url) in [
        ("No file", "images/gone.png".to_owned()),
        ("No page", server.url("/gone.png")),
        ("Not an image", server.url("/notes.txt")),
        ("Not an image file", "images/notes.png".to_owned()),
    ] {
        assert!(
            warnings.iter().any(|w| w.contains(&format!("\"{name}\"")) && w.contains(&url)),
            "no warning about {name}: {warnings:?}"
        );
    }
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    // A grey placeholder where each image would be, off its cross.
    for x in [50, 150, 250, 350] {
        near(&called.images[0], (x, 20), PLACEHOLDER, 2);
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn a_design_tab_is_told_each_images_file_and_never_downloads() {
    let server = Server::start(vec![("/stripes.png", stripes())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Followed images").await;
    let local = path.parent().unwrap().join("images").join("stripes.png");
    std::fs::write(&local, stripes()).unwrap();
    let url = server.url("/stripes.png");
    let snippet = format!(
        r#"Insert({{ type: "rectangle", width: 10, height: 10, fill: [{{ type: "image", url: "images/stripes.png" }},
  {{ type: "image", url: {} }}, {{ type: "image", url: "images/gone.png" }}] }});"#,
        json!(url)
    );
    assert!(!seam.execute(&path, &snippet).await.is_error);

    // The window watches it: each image it has is an asset, named by its
    // url, with its file on this computer; the one it hasn't isn't named,
    // and draws as a placeholder.
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window.subscribe("WatchDesign", json!({ "path": path })).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), seen.recv()).await.unwrap().unwrap();
    let images: Vec<&Value> = first["assets"].as_array().expect("assets").iter().filter(|a| a["kind"] == "image").collect();
    assert_eq!(images.len(), 2, "{first}");
    let file = |url: &str| {
        let image = images.iter().find(|a| a["name"] == url).unwrap_or_else(|| panic!("no {url} in {first}"));
        std::path::PathBuf::from(image["path"].as_str().unwrap())
    };
    assert_eq!(std::fs::read(file("images/stripes.png")).unwrap(), stripes());
    assert_eq!(std::fs::read(file(&url)).unwrap(), stripes());
    assert!(file(&url).is_absolute() && !file(&url).starts_with(path.parent().unwrap()));
    assert_eq!(server.hits("/stripes.png"), 1);
    seam.shutdown().await;
}

#[tokio::test]
async fn an_image_must_be_a_relative_path_or_an_http_url() {
    let seam = Seam::new().await;
    let path = design(&seam, "Refused").await;
    let absolute = path.parent().unwrap().join("images").join("photo.png");
    for url in [
        absolute.to_string_lossy().into_owned(),
        "data:image/png;base64,iVBORw0KGgo=".to_owned(),
        "ftp://example.com/photo.png".to_owned(),
        "images/../../elsewhere/photo.png".to_owned(),
        String::new(),
    ] {
        let snippet = format!(
            r#"Insert({{ type: "rectangle", width: 10, height: 10, fill: {{ type: "image", url: {} }} }});"#,
            json!(url)
        );
        let called = seam.execute(&path, &snippet).await;
        assert!(called.is_error, "{url}: {}", called.text);
        assert!(called.text.contains("relative"), "{url}: {}", called.text);
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn the_saved_file_holds_only_the_images_reference() {
    let server = Server::start(vec![("/stripes.png", stripes())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Saved images").await;
    std::fs::write(path.parent().unwrap().join("images").join("stripes.png"), stripes()).unwrap();
    let url = server.url("/stripes.png");
    let snippet = format!(
        r##"Insert({{ type: "rectangle", name: "Local", width: 100, height: 100,
  fill: {{ type: "image", url: "images/stripes.png", mode: "contain", opacity: 0.5 }} }});
Insert({{ type: "rectangle", name: "Remote", width: 100, height: 100, fill: ["#000000", {{ type: "image", url: {} }}] }});"##,
        json!(url)
    );
    let called = seam.execute(&path, &snippet).await;
    assert!(!called.is_error, "{}", called.text);
    let text = std::fs::read_to_string(&path).unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        file["children"][1]["fill"],
        json!({ "type": "image", "url": "images/stripes.png", "mode": "contain", "opacity": 0.5 })
    );
    // Cover is the default, so it isn't written.
    assert_eq!(file["children"][2]["fill"][1], json!({ "type": "image", "url": url }));
    assert!(text.len() < 1500, "{text}");

    // And read back as written: the next call keeps it.
    let again = seam.execute(&path, r#"Insert({ type: "frame", width: 1, height: 1 });"#).await;
    assert!(!again.is_error, "{}", again.text);
    let reread: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(reread["children"][1], file["children"][1]);
    assert_eq!(reread["children"][2], file["children"][2]);
    seam.shutdown().await;
}

#[tokio::test]
async fn an_image_on_a_stroke_is_kept_and_warned_of() {
    let seam = Seam::new().await;
    let path = design(&seam, "Stroked").await;
    let called = seam
        .execute(
            &path,
            r##"Insert({ type: "rectangle", name: "Framed", width: 10, height: 10,
  stroke: { fill: { type: "image", url: "images/stripes.png" }, thickness: 2 } });"##,
        )
        .await;
    let warned = warnings(&called);
    assert!(warned.iter().any(|w| w.contains("\"Framed\"") && w.contains("only as fills")), "{warned:?}");
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(file["children"][1]["stroke"]["fill"], json!({ "type": "image", "url": "images/stripes.png" }));
    seam.shutdown().await;
}

#[tokio::test]
async fn an_image_named_by_a_token_inside_an_instance_is_downloaded_and_drawn() {
    let server = Server::start(vec![("/stripes.png", stripes())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Token images").await;
    let snippet = format!(
        r##"SetTokens({{ photo: {{ type: "string", value: {} }} }});
card = Insert({{ type: "frame", name: "Card", reusable: true, x: 0, y: 200, width: 100, height: 100,
  children: [{{ type: "rectangle", name: "Photo", width: 100, height: 100, fill: {{ type: "image", url: "$photo", mode: "stretch" }} }}] }});
screen = Insert({{ type: "frame", name: "Screen", width: 100, height: 100, fill: "#FFFFFF" }});
Copy(card, screen, {{ x: 0, y: 0 }});
TakeScreenshot([screen]);"##,
        json!(server.url("/stripes.png"))
    );
    let called = seam.execute(&path, &snippet).await;
    assert_eq!(called.images.len(), 1, "{}", called.text);
    assert!(warnings(&called).is_empty(), "{}", called.text);
    // The stripes, stretched, through the Instance.
    near(&called.images[0], (12, 50), RED, 2);
    near(&called.images[0], (87, 50), BLACK, 2);
    assert_eq!(server.hits("/stripes.png"), 1);
    seam.shutdown().await;
}

#[tokio::test]
async fn save_as_keeps_a_url_image_as_written_and_draws_it_from_the_cache() {
    let server = Server::start(vec![("/stripes.png", stripes())]);
    let seam = Seam::new().await;
    let path = design(&seam, "Moved").await;
    let (called, _) = three_modes(&seam, &path, &server.url("/stripes.png")).await;
    let screen = called.id("Modes");

    let project = tempfile::tempdir().unwrap();
    let to = project.path().join("Moved.zdesign");
    let saved = seam.window("SaveDesignAs", json!({ "path": path, "to": to })).await.unwrap();
    let to = std::path::PathBuf::from(saved["path"].as_str().unwrap());
    // Nothing downloaded is copied beside it; the url is kept as written.
    let copied = std::fs::read_dir(to.parent().unwrap().join("images"));
    assert!(copied.map_or(true, |mut files| files.next().is_none()));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&to).unwrap()).unwrap();
    assert_eq!(file["children"][1]["children"][0]["fill"]["url"], json!(server.url("/stripes.png")));
    let again = seam.execute(&to, &format!("TakeScreenshot({});", json!(screen))).await;
    assert_eq!(again.images.len(), 1, "{}", again.text);
    drawn_in_three_modes(&again.images[0]);
    assert_eq!(server.hits("/stripes.png"), 1);
    seam.shutdown().await;
}
