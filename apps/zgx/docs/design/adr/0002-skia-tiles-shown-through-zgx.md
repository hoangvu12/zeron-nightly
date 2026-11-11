# Designs are drawn by Skia on the CPU and shown through zgx

A Design is rendered by Skia (skia-safe, raster backend) in a Zeron crate, into tiles that the zgx Design tab displays as images, with zgx's own elements drawing selection, handles and panels on top. Agent screenshots come from the same crate, so they match what the person sees. We rejected translating Designs into zgx elements (about 80–88% of pen.dev's look: no per-side outer strokes, true angular gradients or gradient strokes, and capped background blur), and rejected Skia or vello on the GPU because sharing zgx's device needs a zgx fork.

## Consequences

- Each Zeron binary that renders grows by about 17 MB. No prebuilt exists for the raster-only feature set, so we pin `skia-safe = "=0.157.0"` with `gl`, `textlayout` and `svg` to match a published one; the GL backend is linked but unused.
- Checked on a 2,928-node design: no blank tiles while panning, sharp within 50 ms after zoom, drag at 90+ updates a second. zgx uploads every replaced image whole on its main thread, which shows as zoom hitches on HiDPI displays; we accept that rather than fork zgx, until real use says otherwise.
- The renderer sits behind an interface so a tiny-skia renderer can replace it without touching the rest.
