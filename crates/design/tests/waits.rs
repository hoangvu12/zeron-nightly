//! A snippet's waits on the network (fetching icons) stop at its call's
//! cap, so a slow network can't hang a call.
#![cfg(feature = "snippet")]

use std::sync::Arc;
use std::time::Duration;

use zeron_design::Design;
use zeron_design::assets::Cache;
use zeron_design::edit::Draft;
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
fn icon_fetches_stop_once_a_call_has_waited_its_cap() {
    let dir = std::env::temp_dir().join(format!("zeron-design-waits-{}", uuid::Uuid::new_v4()));
    let assets = Arc::new(Cache::new(&dir, Arc::new(Slow(Duration::from_millis(600)))));
    let limits = Limits { fetching: Duration::from_secs(1), ..Limits::default() };
    let draft = Draft::new(Design::default()).with_assets(assets);
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
    let _ = std::fs::remove_dir_all(dir);
}
