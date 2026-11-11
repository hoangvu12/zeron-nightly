//! Zeron Design: the Design model ([`model`]), changing it ([`edit`], and
//! agents' JavaScript snippets with the `snippet` feature), laying it out
//! ([`layout`]), reading it back ([`read`]), finding room on it ([`space`]),
//! undoing it ([`history`]) and drawing it ([`render`]) with
//! Skia on the CPU raster backend
//! (apps/zgx/docs/design/adr/0002). Shared by Zeron's engine (agents'
//! edits, screenshots and exports) and its zgx app (the Design tab's
//! tiles), so the two draw alike.

pub mod assets;
pub mod edit;
pub mod history;
pub mod icons;
pub mod layout;
pub mod model;
pub mod printed;
pub mod read;
pub mod render;
pub mod skia;
#[cfg(feature = "snippet")]
pub mod snippet;
pub mod space;
pub mod waits;

pub use model::Design;
