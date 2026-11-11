//! A snippet's waits on the network (fetching icons, fonts and images)
//! stop at its call's cap, so a slow network can't hang a call, and don't
//! count against its running time.
#![cfg(feature = "snippet")]

use std::sync::Arc;
use std::time::Duration;

use zeron_design::Design;
use zeron_design::assets::Cache;
use zeron_design::edit::Draft;
use zeron_design::fonts::{FontSource, Fonts};
use zeron_design::icons::IconSource;
use zeron_design::images::ImageSource;
use zeron_design::snippet::{self, Globals, Limits};

/// Any icon, after a wait as long as it holds.
struct Slow(Duration);

impl IconSource for Slow {
    fn fetch(&self, _: &str, _: &str) -> Result<Option<String>, String> {
        std::thread::sleep(self.0);
        Ok(Some(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"/>"#.into()))
    }
}

#[test]
fn icon_fetches_stop_once_a_call_has_waited_its_cap_and_the_next_call_may_fetch_again() {
    let dir = std::env::temp_dir().join(format!("zeron-design-waits-{}", uuid::Uuid::new_v4()));
    let assets = Arc::new(Cache::new(&dir, Arc::new(Slow(Duration::from_millis(600)))));
    let limits = Limits { fetching: Duration::from_secs(1), ..Limits::default() };
    let draft = Draft::new(Design::default()).with_assets(assets.clone());
    let failed = snippet::run(
        draft,
        Globals::default(),
        r#"for (const name of ["a", "b", "c", "d"]) Insert({ type: "icon", icon: "set:" + name, width: 24, height: 24 });"#,
        limits,
    )
    .err()
    .expect("the third fetch is refused");
    let said = failed.to_string();
    assert!(said.contains("set:c") && said.contains("waited 1s"), "{said}");

    // The next call has its own time: it fetches what this one couldn't.
    let draft = Draft::new(Design::default()).with_assets(assets);
    let code = r#"Insert({ type: "icon", icon: "set:c", width: 24, height: 24 });"#;
    assert!(snippet::run(draft, Globals::default(), code, limits).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}

/// Instrument Serif, after a wait as long as it holds.
struct SlowFont(Duration);

impl FontSource for SlowFont {
    fn fetch(&self, _: &str, _: u16) -> Result<Option<Vec<u8>>, String> {
        std::thread::sleep(self.0);
        Ok(Some(include_bytes!("../../../apps/zgx/fonts/InstrumentSerif-Regular.ttf").to_vec()))
    }
}

#[test]
fn a_font_fetch_doesnt_count_against_the_snippets_time() {
    // Fetched as Get lays the text out: 600 ms on the network in a call
    // given 200 ms to run.
    let fonts = Fonts::new(None, Some(Box::new(SlowFont(Duration::from_millis(600)))));
    let limits = Limits { time: Duration::from_millis(200), ..Limits::default() };
    let draft = Draft::new(Design::default()).with_fonts(fonts);
    let code = r##"
Insert({ type: "text", content: "Hello", fontFamily: "Instrument Serif", fill: "#000000" });
Get((node, info) => Print(info.bounds.width));"##;
    let (draft, _) = snippet::run(draft, Globals::default(), code, limits).expect("the fetch's wait isn't its time");
    let width: f32 = draft.finish().printed[0].parse().unwrap();
    assert!(width > 0., "{width}");
}

/// A 2 × 2 PNG at any URL, after a wait as long as it holds.
struct SlowImages(Duration);

impl ImageSource for SlowImages {
    fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
        std::thread::sleep(self.0);
        let mut surface = zeron_design::skia::surface(2, 2).expect("a surface");
        Ok(zeron_design::skia::png(&mut surface).expect("a PNG"))
    }
}

#[test]
fn image_downloads_wait_off_the_snippets_time_and_past_the_cap_leave_the_call_be() {
    let dir = std::env::temp_dir().join(format!("zeron-design-waits-{}", uuid::Uuid::new_v4()));
    let assets = Arc::new(Cache::new(&dir, Arc::new(Slow(Duration::ZERO))));
    assets.set_image_source(Arc::new(SlowImages(Duration::from_millis(600))));
    let place = |urls: &[&str]| {
        urls.iter()
            .map(|url| format!(r#"Insert({{ type: "rectangle", width: 10, height: 10, fill: {{ type: "image", url: "{url}" }} }});"#))
            .collect::<String>()
    };
    // Three downloads, 1.8 s of waiting, in a call with 1 s to run.
    let limits = Limits { time: Duration::from_secs(1), fetching: Duration::from_secs(10), ..Limits::default() };
    let urls = ["https://example.com/a.png", "https://example.com/b.png", "https://example.com/c.png"];
    let draft = Draft::new(Design::default()).with_assets(assets.clone());
    assert!(snippet::run(draft, Globals::default(), &place(&urls), limits).is_ok());
    for url in urls {
        assert!(assets.images().file_of(url).is_file(), "{url} wasn't cached");
    }

    // Past its cap, a call doesn't download the rest, and isn't failed for
    // it: those draw as placeholders, and a later call downloads them.
    let limits = Limits { fetching: Duration::from_secs(1), ..Limits::default() };
    let more = ["https://example.com/d.png", "https://example.com/e.png", "https://example.com/f.png", "https://example.com/g.png"];
    let draft = Draft::new(Design::default()).with_assets(assets.clone());
    assert!(snippet::run(draft, Globals::default(), &place(&more), limits).is_ok());
    assert!(!assets.images().file_of(more[3]).is_file());
    // Nor is it downloaded to say why it isn't drawn.
    let why = assets.beside(&dir).image(more[3]).expect_err("not downloaded");
    assert!(why.contains("next call"), "{why}");
    assert!(!assets.images().file_of(more[3]).is_file());
    let draft = Draft::new(Design::default()).with_assets(assets.clone());
    assert!(snippet::run(draft, Globals::default(), &place(&more[3..]), limits).is_ok());
    assert!(assets.images().file_of(more[3]).is_file());
    let _ = std::fs::remove_dir_all(dir);
}
