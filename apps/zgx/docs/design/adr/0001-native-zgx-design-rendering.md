# Designs are rendered inside Zeron, not by a bundled engine

Zeron draws Designs and the agents' screenshots of them itself, shipped as patches on the nightly, rather than as a plugin bundling a separate Skia engine and a WebView2 viewer. The bundled engine would have matched pen.dev's output with no Zeron patches. We chose to build it into Zeron so the Canvas is part of Zeron itself, at the cost of patches to carry on the nightly.

## Consequences

- The renderer is recorded in ADR-0002.
- Agent tools are Zeron's own MCP tools, which know the calling chat, so a chat's agent acts on the Design open in that chat's tab.
