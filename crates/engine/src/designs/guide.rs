//! The design guide agents design with: the core guide (the rules, the
//! snippet API, the file format and how to check work), which a chat gets
//! with its first design call, and topic guides it asks `design_guide` for.
//! Each is Markdown under `guide/`; every ```js block in them is a snippet
//! that runs as written (the engine's `design` tests run them all).

/// The core guide.
pub const CORE: &str = include_str!("guide/core.md");

/// The topic guides, by the name `design_guide` takes.
pub const TOPICS: &[(&str, &str)] = &[
    ("components", include_str!("guide/components.md")),
    ("design-systems", include_str!("guide/design-systems.md")),
    ("landing-pages", include_str!("guide/landing-pages.md")),
    ("mobile-apps", include_str!("guide/mobile-apps.md")),
    ("web-apps", include_str!("guide/web-apps.md")),
    ("slides", include_str!("guide/slides.md")),
    ("tables", include_str!("guide/tables.md")),
    ("code", include_str!("guide/code.md")),
    ("tailwind", include_str!("guide/tailwind.md")),
];

/// The topic guide `name`.
pub fn topic(name: &str) -> Option<&'static str> {
    TOPICS.iter().find(|(topic, _)| *topic == name).map(|(_, guide)| *guide)
}

/// The topics' names.
pub fn topics() -> Vec<&'static str> {
    TOPICS.iter().map(|(topic, _)| *topic).collect()
}
