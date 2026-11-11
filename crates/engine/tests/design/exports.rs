//! Exports (#29): nodes as PNG files at a scale, Screens as HTML with plain
//! CSS or Tailwind, and Tokens as CSS custom properties per Theme.

use std::path::Path;

use serde_json::Value;

use crate::seam::Seam;

/// A 40 × 20 PNG of two upright stripes: red, then blue.
fn stripes() -> Vec<u8> {
    let image = image::RgbaImage::from_fn(40, 20, |x, _| {
        image::Rgba(if x < 20 { [255, 0, 0, 255] } else { [0, 0, 255, 255] })
    });
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    png.into_inner()
}

/// A Screen as an implementation would start from it: themed Tokens, a
/// column card with a title, an image, an icon and an Instance of a
/// Component (a button), the second Screen shown dark.
const SHOP: &str = r##"
SetTokens({
  surface: { type: "color", value: [{ value: "#FFFFFF", theme: { mode: "light" } }, { value: "#111827", theme: { mode: "dark" } }] },
  accent: { type: "color", value: "#4F46E5" },
  gap: { type: "number", value: 12 },
});
button = Insert({ type: "frame", name: "Button", reusable: true, x: 0, y: 700, layout: "horizontal", padding: [8, 16],
  fill: "$accent", cornerRadius: 8,
  children: [{ type: "text", name: "Label", content: "Buy now", fontFamily: "Inter", fontWeight: 600, fontSize: 14, fill: "#FFFFFF" }] });
screen = Insert({ type: "frame", name: "Shop", x: 0, y: 0, width: 400, height: 600, fill: "$surface", layout: "vertical",
  gap: "$gap", padding: 24, alignItems: "center" });
Insert(screen, { type: "text", name: "Title", content: "Fresh <picks> & more", fontSize: 28, fontWeight: 700, fill: "#111827",
  letterSpacing: -0.5 });
Insert(screen, { type: "rectangle", name: "Photo", width: "fill_container", height: 200, cornerRadius: 12,
  fill: { type: "image", url: "images/stripes.png", mode: "cover" } });
Insert(screen, { type: "icon", name: "Cart", icon: "mdi:square", width: 24, height: 24, fill: "#EF4444" });
Copy(button, screen, {});
dark = Insert({ type: "frame", name: "Dark", x: 500, y: 0, width: 200, height: 100, fill: "$surface", theme: { mode: "dark" } });
"##;

/// A Design made from `snippet`, with the stripes in its `images/`.
async fn shop(seam: &Seam) -> std::path::PathBuf {
    let path = seam.create("Shop", "1;").await.path();
    std::fs::write(path.parent().unwrap().join("images/stripes.png"), stripes()).unwrap();
    let made = seam.execute(&path, SHOP).await;
    assert!(!made.is_error, "{}", made.text);
    path
}

fn exported(called: &crate::seam::Called) -> Vec<String> {
    serde_json::from_value(called.json()["exported"].clone()).unwrap_or_else(|_| panic!("{}", called.text))
}

#[tokio::test]
async fn png_exports_write_one_file_per_node_at_the_scale_asked() {
    let seam = Seam::new().await;
    let path = shop(&seam).await;
    let folder = path.parent().unwrap();
    let called = seam
        .execute(
            &path,
            r#"Print(Export([screen, dark], { format: "png", path: "exports" }));
Export([dark], { format: "png", path: "exports/1x", scale: 1 });"#,
        )
        .await;
    let files = exported(&called);
    let (shop, dark) = (folder.join("exports").join("Shop.png"), folder.join("exports").join("Dark.png"));
    let small = folder.join("exports").join("1x").join("Dark.png");
    assert_eq!(files, [shop.display().to_string(), dark.display().to_string(), small.display().to_string()]);
    // Export returns the paths in the snippet too.
    let printed: Vec<String> = serde_json::from_str(&called.printed()[0]).unwrap();
    assert_eq!(printed, files[..2]);
    // 2x by default.
    let shop = image::open(&shop).unwrap().to_rgba8();
    assert_eq!(shop.dimensions(), (800, 1200));
    assert_eq!(shop.get_pixel(10, 10).0, [255, 255, 255, 255]);
    let dark = image::open(&dark).unwrap().to_rgba8();
    assert_eq!((dark.dimensions(), dark.get_pixel(20, 20).0), ((400, 200), [17, 24, 39, 255]));
    assert_eq!(image::open(&small).unwrap().to_rgba8().dimensions(), (200, 100));

    let too_big = seam.execute(&path, r#"Export([dark], { format: "png", path: "x", scale: 20 });"#).await;
    assert!(too_big.failure()["error"].as_str().unwrap().contains("scale"), "{}", too_big.text);
    seam.shutdown().await;
}

#[tokio::test]
async fn html_exports_lay_screens_out_with_plain_css_or_tailwind() {
    let seam = Seam::new().await;
    let path = shop(&seam).await;
    let folder = path.parent().unwrap();
    let called = seam
        .execute(
            &path,
            r#"Export([screen, dark], { format: "html", path: "site/shop.html", layerNames: true });
Export([screen], { format: "html", path: "site/tailwind.html", css: "tailwind" });
Export({ format: "html", path: "site/part.html", scaffold: false });"#,
        )
        .await;
    assert_eq!(exported(&called).len(), 3, "{}", called.text);
    let plain = std::fs::read_to_string(folder.join("site/shop.html")).unwrap();

    // A page, with the fonts its text uses and its Tokens per Theme.
    assert!(plain.starts_with("<!doctype html>"), "{plain}");
    assert!(plain.contains("https://fonts.googleapis.com/css2?family=Geist:wght@400;700&family=Inter:wght@600&display=swap")
        || plain.contains("family=Inter:wght@600"), "{plain}");
    assert!(plain.contains("--surface: #FFFFFF;") && plain.contains("--gap: 12;"), "{plain}");
    assert!(plain.contains("[data-mode=\"dark\"] {\n") && plain.contains("--surface: #111827;"), "{plain}");
    assert!(plain.contains("data-mode=\"dark\""), "the dark Screen carries its Theme");
    // The Screen a column, laid out as the Design does, and its nodes named.
    for css in [
        "display: flex;", "flex-direction: column;", "gap: calc(var(--gap) * 1px);", "padding: 24px 24px 24px 24px;",
        "align-items: center;", "background-color: var(--surface);", "width: 400px;", "letter-spacing: -0.5px;",
        "border-radius: 12px;", "flex-shrink: 0;", "align-self: stretch;",
    ] {
        assert!(plain.contains(css), "no {css} in {plain}");
    }
    assert!(plain.contains("data-name=\"Shop\"") && plain.contains("data-name=\"Title\""), "{plain}");
    // Text escaped, the image from the page, the icon inline.
    assert!(plain.contains(">Fresh &lt;picks&gt; &amp; more</p>"), "{plain}");
    assert!(plain.contains("url(\"../images/stripes.png\") center / cover no-repeat"), "{plain}");
    assert!(plain.contains("<svg width=\"100%\" height=\"100%\""), "{plain}");
    // The Instance as its Component draws it, the Token it names kept.
    assert!(plain.contains(">Buy now</p>") && plain.contains("background-color: var(--accent);"), "{plain}");

    let tailwind = std::fs::read_to_string(folder.join("site/tailwind.html")).unwrap();
    assert!(tailwind.contains("<script src=\"https://cdn.tailwindcss.com\"></script>"), "{tailwind}");
    for class in ["flex flex-col", "gap-[calc(var(--gap)_*_1px)]", "items-center", "bg-[color:var(--surface)]", "w-[400px]", "self-stretch"] {
        assert!(tailwind.contains(class), "no {class} in {tailwind}");
    }
    assert!(!tailwind.contains("data-name="), "layer names only on request");

    // Without the scaffold, the Screens and their styles alone: every
    // Screen, as none were named, but not the Component.
    let part = std::fs::read_to_string(folder.join("site/part.html")).unwrap();
    assert!(!part.contains("<!doctype") && part.contains("<style>"), "{part}");
    assert_eq!(part.matches(">Buy now</p>").count(), 1, "{part}");
    seam.shutdown().await;
}

#[tokio::test]
async fn tokens_read_as_css_custom_properties_per_theme() {
    let seam = Seam::new().await;
    let path = shop(&seam).await;
    let called = seam.execute(&path, "Print(GetTokens({ css: true }));").await;
    assert_eq!(
        called.printed()[0],
        ":root {\n  --accent: #4F46E5;\n  --gap: 12;\n  --surface: #FFFFFF;\n}\n\
         [data-mode=\"light\"] {\n  --surface: #FFFFFF;\n}\n\
         [data-mode=\"dark\"] {\n  --surface: #111827;\n}\n"
    );
    seam.shutdown().await;
}

#[tokio::test]
async fn exports_where_nothing_can_be_written_fail_the_call_clearly() {
    let seam = Seam::new().await;
    let path = shop(&seam).await;
    let folder = path.parent().unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    // A file where the folder would be.
    std::fs::write(folder.join("taken"), b"a file").unwrap();
    let blocked = seam
        .execute(&path, r#"Update(screen, { width: 10 }); Export([screen], { format: "png", path: "taken/pngs" });"#)
        .await;
    let error = blocked.failure()["error"].as_str().unwrap().to_owned();
    assert!(error.contains("can't make the folder") && error.contains("taken"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "the call's edits went with it");
    // A call that fails after its Export leaves no folder behind.
    let later = seam
        .execute(&path, r#"Export([screen], { format: "png", path: "later/pngs" }); throw new Error("no");"#)
        .await;
    assert!(later.is_error && !folder.join("later").exists(), "{}", later.text);
    // A path through `..` finds the Design's images from where it lands.
    let up = seam
        .execute(&path, r#"Export([screen], { format: "html", path: "../elsewhere/up.html" });"#)
        .await;
    let page = folder.parent().unwrap().join("elsewhere").join("up.html");
    assert_eq!(exported(&up), [page.display().to_string()]);
    let html = std::fs::read_to_string(&page).unwrap();
    assert!(html.contains("url(\"../Shop/images/stripes.png\")"), "{html}");

    for (snippet, says) in [
        (r#"Export({ format: "html", path: "page.txt" });"#, ".html file"),
        (r#"Export({ format: "html", path: "Shop.zdesign" });"#, "Design's file"),
        (r#"Export({ format: "pdf", path: "x" });"#, "\"png\" or \"html\""),
        (r#"Export({ format: "png", path: "x" });"#, "ids"),
    ] {
        let refused = seam.execute(&path, snippet).await;
        let error = refused.failure()["error"].as_str().unwrap().to_owned();
        assert!(error.contains(says), "{snippet}: {error}");
    }
    assert!(!Path::new(&folder.join("x")).exists());
    let _: Value = serde_json::from_str(&before).unwrap();
    seam.shutdown().await;
}

/// For the manual browser check: the Shop Screen as PNG (1x) and as HTML
/// with plain CSS and Tailwind, into `ZERON_EXPORT_CHECK` (a folder), to
/// open in a browser beside the PNG.
#[tokio::test]
#[ignore = "writes into a folder named by ZERON_EXPORT_CHECK, for a person to look at"]
async fn browser_check() {
    let folder = std::path::PathBuf::from(std::env::var("ZERON_EXPORT_CHECK").expect("ZERON_EXPORT_CHECK"));
    std::fs::create_dir_all(folder.join("images")).unwrap();
    std::fs::write(folder.join("images/stripes.png"), stripes()).unwrap();
    let path = folder.join("Shop.zdesign");
    std::fs::write(&path, r#"{ "version": "1.0" }"#).unwrap();
    let seam = Seam::new().await;
    let made = seam
        .execute(
            &path,
            &format!(
                r#"{SHOP}
Export([screen], {{ format: "png", path: ".", scale: 1 }});
Export([screen], {{ format: "html", path: "plain.html" }});
Export([screen], {{ format: "html", path: "tailwind.html", css: "tailwind" }});"#
            ),
        )
        .await;
    println!("{}", made.text);
    seam.shutdown().await;
}
