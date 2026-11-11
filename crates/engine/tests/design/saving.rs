//! Saving, engine side (#28), pen.dev's way: managed Designs save
//! themselves a moment after a change; a Design opened from anywhere else
//! changes on disk only when it's saved, its changes kept meanwhile in a
//! backup that's recovered after a crash; Save as takes a Design's images
//! along. The Design tab's Ctrl+S, Save as and Save/Discard prompt (#31)
//! use the window's RPCs these tests call in its place.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::seam::{CHAT, Seam};

const SCREEN: &str = r##"Insert({ type: "frame", name: "Screen", width: 400, height: 300, fill: "#FFFFFF" });"##;

/// A rectangle named `name` on the Canvas.
fn card(name: &str) -> String {
    format!(r##"Insert({{ type: "rectangle", name: "{name}", width: 10, height: 10, fill: "#000000" }});"##)
}

fn on_disk(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// What `DesignSaving` says of the Design at `path`.
async fn saving(seam: &Seam, path: &Path) -> Value {
    seam.window("DesignSaving", json!({ "path": path })).await.unwrap()
}

/// A Design kept in a project folder, outside the managed folder, by the
/// path the engine gives it (Windows' long names, not `HPMEDI~1`).
async fn project_design(seam: &Seam, project: &Path, name: &str) -> PathBuf {
    let made = seam.create(name, SCREEN).await.path();
    let path = project.join(format!("{name}.zdesign"));
    std::fs::copy(made, &path).unwrap();
    PathBuf::from(saving(seam, &path).await["path"].as_str().unwrap())
}

/// The backups Zeron keeps of unsaved changes (and anything kept with
/// them).
fn backups(seam: &Seam) -> Vec<String> {
    std::fs::read_dir(seam.data_dir().join("design-backups"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|e| e == "zdesign"))
                .map(|e| std::fs::read_to_string(e.path()).unwrap())
                .collect()
        })
        .unwrap_or_default()
}

/// Everything in the backups folder.
fn backup_files(seam: &Seam) -> usize {
    std::fs::read_dir(seam.data_dir().join("design-backups")).map_or(0, |entries| entries.count())
}

/// An 8 × 8 PNG of one colour.
fn png(rgb: [u8; 3]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(8, 8, image::Rgb(rgb))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[tokio::test]
async fn managed_designs_save_themselves_within_the_autosave_interval() {
    let seam = Seam::new().await;
    let interval = Duration::from_millis(500);
    seam.core.designs.autosave_after(interval);
    // A new Design is on disk with its first call.
    let path = seam.create("Auto", SCREEN).await.path();
    assert!(path.is_file());

    // A change is unsaved for a moment, then on disk, with any made
    // meanwhile.
    let changed = Instant::now();
    let first = seam.execute(&path, &card("First")).await.id("First");
    let second = seam.execute(&path, &card("Second")).await.id("Second");
    assert!(!on_disk(&path).contains(&first));
    let unsaved = saving(&seam, &path).await;
    assert_eq!((unsaved["managed"].clone(), unsaved["unsaved"].clone()), (json!(true), json!(true)), "{unsaved}");
    while !on_disk(&path).contains(&second) {
        assert!(changed.elapsed() < interval + Duration::from_secs(2), "not saved after {:?}", changed.elapsed());
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(on_disk(&path).contains(&first));
    assert_eq!(saving(&seam, &path).await["unsaved"], false);

    // A managed Design has nothing to discard, and the engine saves what's
    // waiting as it stops.
    assert!(seam.window("DiscardDesign", json!({ "path": path })).await.is_err());
    let last = seam.execute(&path, &card("Last")).await.id("Last");
    let seam = seam.restart().await;
    assert!(on_disk(&path).contains(&last));
    seam.shutdown().await;
}

#[tokio::test]
async fn designs_opened_from_elsewhere_change_on_disk_only_when_saved() {
    let seam = Seam::new().await;
    let project = tempfile::tempdir().unwrap();
    let path = project_design(&seam, project.path(), "Mine").await;
    let before = on_disk(&path);

    let changed = seam.execute(&path, &card("Card")).await;
    let card = changed.id("Card");
    // Told in one short line.
    let notice = changed.json()["unsaved"].as_str().unwrap().to_owned();
    assert!(notice.contains("saves the Design") && notice.len() < 100, "{notice}");
    assert_eq!(on_disk(&path), before);
    let state = saving(&seam, &path).await;
    assert_eq!(
        state,
        json!({ "path": path, "managed": false, "unsaved": true, "recovered": false, "stale": false })
    );
    // The change is in a backup.
    let kept = backups(&seam);
    assert_eq!(kept.len(), 1);
    assert!(kept[0].contains(&card));

    // Saved, it's in the file and the backup is gone.
    let saved = seam.window("SaveDesign", json!({ "path": path })).await.unwrap();
    assert_eq!(saved["unsaved"], false);
    assert!(on_disk(&path).contains(&card));
    assert_eq!(backup_files(&seam), 0);
    seam.shutdown().await;
}

#[tokio::test]
async fn reopening_after_a_crash_restores_the_backup_marked_recovered() {
    let seam = Seam::new().await;
    let project = tempfile::tempdir().unwrap();
    let path = project_design(&seam, project.path(), "Crashed").await;
    let before = on_disk(&path);
    let card = seam.execute(&path, &card("Unsaved")).await.id("Unsaved");

    // Zeron stops without saving it; started again, the Design is as it was
    // with its changes, marked recovered, and the file still as it was.
    let seam = seam.restart().await;
    let state = saving(&seam, &path).await;
    assert_eq!((state["unsaved"].clone(), state["recovered"].clone()), (json!(true), json!(true)), "{state}");
    // Its file is as it was when the changes were made.
    assert_eq!(state["stale"], false);
    let read = seam.execute(&path, &format!(r#"Print(Get("{card}", {{ depth: 0 }}).name);"#)).await;
    assert_eq!(read.printed(), ["Unsaved"]);
    assert_eq!(on_disk(&path), before);

    // Discarded, it's the file's again (one undo step), no longer
    // recovered, and its backup is gone.
    let discarded = seam.window("DiscardDesign", json!({ "path": path })).await.unwrap();
    assert_eq!((discarded["unsaved"].clone(), discarded["recovered"].clone()), (json!(false), json!(false)));
    let gone = seam
        .execute(
            &path,
            &format!(r#"let there; try {{ there = Get("{card}", {{ depth: 0 }}) != null; }} catch (e) {{ there = false; }} Print(there ? "there" : "gone");"#),
        )
        .await;
    assert_eq!(gone.printed(), ["gone"]);
    seam.window("SaveDesign", json!({ "path": path })).await.unwrap();
    assert!(!on_disk(&path).contains(&card));
    assert!(backups(&seam).is_empty());

    // Saving a recovered Design ends it being recovered too.
    seam.execute(&path, &self::card("Again")).await.json();
    let seam = seam.restart().await;
    assert_eq!(saving(&seam, &path).await["recovered"], true);
    let saved = seam.window("SaveDesign", json!({ "path": path })).await.unwrap();
    assert_eq!((saved["unsaved"].clone(), saved["recovered"].clone()), (json!(false), json!(false)));
    seam.shutdown().await;
}

#[tokio::test]
async fn recovered_changes_to_a_file_changed_on_disk_since_are_stale() {
    let seam = Seam::new().await;
    let project = tempfile::tempdir().unwrap();
    let path = project_design(&seam, project.path(), "Pulled").await;
    let card = seam.execute(&path, &card("Unsaved")).await.id("Unsaved");
    // More changes after the first don't move what the backup began from.
    seam.execute(&path, &self::card("More")).await.json();

    // Zeron stops; the file changes on disk meanwhile (a `git pull`).
    let seam = seam.restart().await;
    let pulled = seam.create("Pulled elsewhere", SCREEN).await.path();
    std::fs::copy(&pulled, &path).unwrap();

    // Still recovered, and stale, here and to a tab following it.
    let state = saving(&seam, &path).await;
    assert_eq!(
        (state["recovered"].clone(), state["stale"].clone()),
        (json!(true), json!(true)),
        "{state}"
    );
    let read = seam.execute(&path, &format!(r#"Print(Get("{card}", {{ depth: 0 }}).name);"#)).await;
    assert_eq!(read.printed(), ["Unsaved"]);
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window.subscribe("WatchDesign", json!({ "path": path })).await.unwrap();
    assert_eq!(next(&mut seen).await["stale"], true);

    // Saved, neither, and to the tab too.
    let saved = seam.window("SaveDesign", json!({ "path": path })).await.unwrap();
    assert_eq!((saved["recovered"].clone(), saved["stale"].clone()), (json!(false), json!(false)));
    assert_eq!(next(&mut seen).await["stale"], false);
    assert_eq!(backup_files(&seam), 0);

    // A change after that measures from the file as saved: not stale.
    seam.execute(&path, &self::card("Later")).await.json();
    let seam = seam.restart().await;
    let state = saving(&seam, &path).await;
    assert_eq!(
        (state["recovered"].clone(), state["stale"].clone()),
        (json!(true), json!(false)),
        "{state}"
    );
    seam.shutdown().await;
}

#[tokio::test]
async fn save_as_copies_the_images_and_the_copy_draws_the_same() {
    const RED: [u8; 3] = [239, 68, 68];
    const BLUE: [u8; 3] = [59, 130, 246];
    const GREEN: [u8; 3] = [34, 197, 94];
    let seam = Seam::new().await;
    let made = seam.create("Poster", SCREEN).await;
    let (old, screen) = (made.path(), made.id("Screen"));
    let folder = old.parent().unwrap();
    std::fs::write(folder.join("images").join("photo.png"), png(RED)).unwrap();
    std::fs::create_dir_all(folder.join("art")).unwrap();
    std::fs::write(folder.join("art").join("badge.png"), png(BLUE)).unwrap();
    let drawn = seam
        .execute(
            &old,
            &format!(
                r##"Insert("{screen}", {{ type: "rectangle", name: "Photo", width: 100, height: 100, fill: {{ type: "image", url: "images/photo.png" }} }});
Insert("{screen}", {{ type: "rectangle", name: "Badge", x: 120, width: 100, height: 100, fill: {{ type: "image", url: "art\\badge.png" }} }});
TakeScreenshot("{screen}");"##
            ),
        )
        .await;
    assert_eq!(drawn.images.len(), 1, "{}", drawn.text);
    seam.core.designs.set_tabs(CHAT, vec![old.clone()]);

    // Into a project that has an images/photo.png of its own.
    let project = tempfile::tempdir().unwrap();
    let theirs = project.path().join("images").join("photo.png");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    std::fs::write(&theirs, png(GREEN)).unwrap();
    let to = project.path().join("design").join("Poster.zdesign");
    let saved = seam
        .window("SaveDesignAs", json!({ "path": old, "to": to }))
        .await
        .unwrap();
    let to = PathBuf::from(saved["path"].as_str().unwrap());
    assert_eq!(saved["managed"], false);
    assert_eq!(saved["unsaved"], false);
    assert!(to.ends_with("design/Poster.zdesign") && to.is_file());

    // Each image at its place beside the copy; the project's own untouched
    // (it's beside the folder, not in it).
    let beside = to.parent().unwrap();
    assert_eq!(std::fs::read(beside.join("images").join("photo.png")).unwrap(), png(RED));
    assert_eq!(std::fs::read(beside.join("art").join("badge.png")).unwrap(), png(BLUE));
    assert_eq!(std::fs::read(&theirs).unwrap(), png(GREEN));
    // The managed Design is left as it was saved.
    assert!(old.is_file());

    // Its tab and the recent list follow it.
    let state = seam.call("design_state", json!({})).await.json();
    assert_eq!(state["designs"][0]["path"], json!(to), "{state}");
    let recent: Vec<_> = state["recent"].as_array().unwrap().iter().map(|r| r["path"].clone()).collect();
    assert!(recent.contains(&json!(to)) && !recent.contains(&json!(old)), "{state}");

    // The copy, opened by an engine started afresh, draws the same.
    let seam = seam.restart().await;
    let again = seam.execute(&to, &format!(r#"TakeScreenshot("{screen}");"#)).await;
    assert_eq!(again.images.len(), 1, "{}", again.text);
    let (before, after) = (&drawn.images[0], &again.images[0]);
    assert_eq!((before.width, before.height), (after.width, after.height));
    for (x, y) in (0..before.width).flat_map(|x| (0..before.height).map(move |y| (x, y))) {
        assert_eq!(before.rgba(x, y), after.rgba(x, y), "at ({x}, {y})");
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn save_as_renames_an_image_another_file_has_the_name_of() {
    const RED: [u8; 3] = [239, 68, 68];
    const GREEN: [u8; 3] = [34, 197, 94];
    let seam = Seam::new().await;
    let made = seam.create("Clash", SCREEN).await;
    let (old, screen) = (made.path(), made.id("Screen"));
    std::fs::write(old.parent().unwrap().join("images").join("photo.png"), png(RED)).unwrap();
    let drawn = seam
        .execute(
            &old,
            &format!(
                r##"Insert("{screen}", {{ type: "rectangle", name: "Photo", width: 100, height: 100, fill: {{ type: "image", url: "images/photo.png" }} }});
TakeScreenshot("{screen}");"##
            ),
        )
        .await;

    // Saved beside another Design's images/photo.png.
    let project = tempfile::tempdir().unwrap();
    let theirs = project.path().join("images").join("photo.png");
    std::fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    std::fs::write(&theirs, png(GREEN)).unwrap();
    let saved = seam
        .window("SaveDesignAs", json!({ "path": old, "to": project.path().join("Clash.zdesign") }))
        .await
        .unwrap();
    let to = PathBuf::from(saved["path"].as_str().unwrap());
    assert_eq!(std::fs::read(&theirs).unwrap(), png(GREEN));
    assert_eq!(std::fs::read(project.path().join("images").join("photo-2.png")).unwrap(), png(RED));
    let text = on_disk(&to);
    assert!(text.contains("images/photo-2.png") && !text.contains("\"images/photo.png\""), "{text}");

    let seam = seam.restart().await;
    let again = seam.execute(&to, &format!(r#"TakeScreenshot("{screen}");"#)).await;
    let (before, after) = (&drawn.images[0], &again.images[0]);
    for (x, y) in (0..before.width).flat_map(|x| (0..before.height).map(move |y| (x, y))) {
        assert_eq!(before.rgba(x, y), after.rgba(x, y), "at ({x}, {y})");
    }
    seam.shutdown().await;
}

#[tokio::test]
async fn save_as_over_a_file_with_a_backup_leaves_nothing_to_recover() {
    let seam = Seam::new().await;
    let project = tempfile::tempdir().unwrap();
    // Zeron stopped with changes to the project's Design unsaved.
    let path = project_design(&seam, project.path(), "Over").await;
    seam.execute(&path, &card("Stale")).await.json();
    let seam = seam.restart().await;

    // Another Design saved over it: what's there now is that one, not
    // recovered, and nothing else lands beside it.
    let other = seam.create("Other", SCREEN).await.path();
    let saved = seam.window("SaveDesignAs", json!({ "path": other, "to": path })).await.unwrap();
    assert_eq!((saved["unsaved"].clone(), saved["recovered"].clone()), (json!(false), json!(false)));
    assert!(backups(&seam).is_empty());
    assert!(!project.path().join("images").exists());
    let seam = seam.restart().await;
    assert_eq!(saving(&seam, &path).await["recovered"], false);
    assert!(!on_disk(&path).contains("Stale"));
    seam.shutdown().await;
}

/// The next thing a watch says, within a few seconds.
async fn next(seen: &mut tokio::sync::mpsc::Receiver<Value>) -> Value {
    tokio::time::timeout(Duration::from_secs(5), seen.recv())
        .await
        .expect("the watch said nothing")
        .expect("the watch ended")
}

#[tokio::test]
async fn unsaved_state_is_reported_per_design() {
    let seam = Seam::new().await;
    let project = tempfile::tempdir().unwrap();
    let mine = project_design(&seam, project.path(), "Mine").await;
    let theirs = project_design(&seam, project.path(), "Theirs").await;
    let managed = seam.create("Managed", SCREEN).await.path();

    seam.execute(&mine, &card("Card")).await.json();
    assert_eq!(saving(&seam, &mine).await["unsaved"], true);
    assert_eq!(saving(&seam, &theirs).await["unsaved"], false);
    assert_eq!(saving(&seam, &managed).await["unsaved"], false);

    // A Design tab following one hears it too.
    let window = zeron_rpc::memory_client(seam.core.rpc_service());
    let mut seen = window.subscribe("WatchDesign", json!({ "path": theirs })).await.unwrap();
    let first = next(&mut seen).await;
    assert_eq!((first["unsaved"].clone(), first["path"].clone()), (json!(false), json!(theirs)));
    seam.execute(&theirs, &card("Card")).await.json();
    assert_eq!(next(&mut seen).await["unsaved"], true);
    seam.window("SaveDesign", json!({ "path": theirs })).await.unwrap();
    let saved = next(&mut seen).await;
    assert_eq!(saved["unsaved"], false);
    assert_eq!(saved["changes"], json!([]), "{saved}");
    assert_eq!(saving(&seam, &mine).await["unsaved"], true);
    seam.shutdown().await;
}
