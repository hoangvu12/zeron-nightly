//! A snippet's waits on the network (fetching icons and fonts) stop at its
//! call's cap, so a slow network can't hang a call, and don't count against
//! its running time.
#![cfg(feature = "snippet")]

use std::sync::Arc;
use std::time::Duration;

use zeron_design::Design;
use zeron_design::assets::Cache;
use zeron_design::edit::Draft;
use zeron_design::fonts::{FontSource, Fonts};
use zeron_design::icons::IconSource;
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
