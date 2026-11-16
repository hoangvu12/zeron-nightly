# ADR 0004: Sandbox providers are built into the engine

- Status: Accepted
- Date: 2026-11-16

Sandbox providers (Docker Sandbox, and Boat on the user's own boat.dev account) are Rust code in the engine (`crates/engine`), behind one provider trait. Other devices reach the host device's provider through the engine's existing request forwarding to a target device (`targetDeviceId`), the same path files and terminals use.

We first planned providers as Luau plugins reached with peer plugin calls (`CallPluginService` → `ServePluginCall`). That doesn't work for the host device we want: built-in plugins load only when the desktop app registers them, so an always-on `zeron headless` host device has none, and they are forgotten on restart. A peer plugin call also needs the identical plugin and version on both devices, and the phone and the stable app can't make one. Plugins also couldn't drive Docker's streaming process API: `zeron.fetch` buffers whole text bodies with a 300 s cap.

The costs, accepted:

- **Adding a provider means an engine release.** Third-party providers as plugins come later, once plugins can run on a headless engine and stream.
- **The engine grows** by one adapter per provider; each keeps every HTTP call in one module, tested against a local stub.
