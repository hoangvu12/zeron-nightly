# Code: implementing a Design

How a coding agent turns a Design into the project's code. You read the Design with snippets that only read and Print (they change nothing, so they leave no undo steps the person would see as changes), then write code in the project's own stack.

## Rules

- Use the project's framework, styling approach, component library and versions. Look at how the codebase does things before writing anything.
- Reuse first: extend an existing component rather than duplicating it, and keep its behaviour working.
- One component at a time: read its full tree, implement it, compare with a screenshot, then move to the next.
- Match exactly: the copy, icons (the same Iconify set and name, through the project's icon package where it has one), spacing, radii, font families, sizes and weights, colours.
- Colours and other Token values go through CSS custom properties named after the Tokens, never pasted hex.
- Path geometry is copied as SVG path data, not redrawn.
- Write code, not documents: no summary files the person didn't ask for.

The examples below read this small sample Design (a Button Component and a Screen using it); run them against yours by changing the ids.

```js
SetTokens({
  "primary": { type: "color", value: [{ value: "#4338CA", theme: { mode: "light" } }, { value: "#A5B4FC", theme: { mode: "dark" } }] },
  "on-primary": { type: "color", value: [{ value: "#FFFFFF", theme: { mode: "light" } }, { value: "#1E1B4B", theme: { mode: "dark" } }] },
  "radius": { type: "number", value: 8 },
});
button = Insert({ type: "frame", name: "Button", reusable: true, x: 0, y: 0, layout: "horizontal", gap: 6, padding: [8, 14], cornerRadius: "$radius",
  fill: "$primary", alignItems: "center", children: [{ type: "text", name: "Label", content: "Save", fontSize: 14, fontWeight: 600, fill: "$on-primary" }] });
screen = Insert({ type: "frame", name: "Settings", x: 0, y: 120, width: 480, layout: "vertical", gap: 16, padding: 24, fill: "#FFFFFF" });
Copy(button, screen, {});
Copy(button, screen, { descendants: { Label: { content: "Save and close" } } });
```

## 1. Inventory

Start with what's there: the Screens, the Components, and how often each Component is used.

```js
const canvas = Get();
const components = canvas.filter((node) => node.reusable);
const uses = {};
Get(null, (node) => { if (node.type === "instance") uses[node.ref] = (uses[node.ref] ?? 0) + 1; });
Print("Screens:", canvas.filter((node) => node.type === "frame" && !node.reusable).map((node) => `${node.id} ${node.name}`));
Print("Components:", components.map((node) => `${node.id} ${node.name} used ${uses[node.id] ?? 0}x`));
```

Count the Instances again in your code when you're done: each should be a use of your component.

## 2. Tokens

`GetTokens({css: true})` gives every Token as a CSS custom property (`--name`, a `.` or `/` in the name becoming `-`; numbers without units): the default Theme's values on `:root` and each other Theme's under a data attribute (`[data-mode="dark"]`). Put that into the project's global stylesheet (or map it onto the project's existing variables).

```js
Print(GetTokens({ css: true }));
```

## 3. Each component

Read the Component whole with Tokens resolved, so you see the values you'll write; read it again without resolving to see which Tokens each property uses (the custom property names you'll write).

```js
Print(JSON.stringify(Get(button, { depth: Infinity, resolveTokens: true })));
Print(JSON.stringify(Get(button, { depth: Infinity })));
```

Then its Instances: each Instance's overrides (`descendants`, and its own properties) are its props. A property overridden somewhere becomes a prop; a node some Instance hides (`enabled: false`) becomes an optional part.

```js
Get(screen, (node) => {
  if (node.type !== "instance") return;
  const { id, ref, descendants, ...own } = node;
  Print(id, "of", ref, JSON.stringify({ own, descendants }));
});
Print(JSON.stringify(Get(screen, { depth: Infinity, expandInstances: true, resolveTokens: true })));
```

How the model maps to CSS:

- `layout: "horizontal" | "vertical"` is `display: flex` with `flex-direction: row | column`; `gap` and `padding` as written; `justifyContent` and `alignItems` as their CSS namesakes (`space_between` is `space-between`).
- `fill_container` is `flex: 1 1 0` along the parent's direction and `align-self: stretch` across it; `fit_content` is the element's natural size; a number is a fixed size.
- A child in a Frame without a layout, or one marked `absolute`, is `position: absolute` at its `x` and `y`.
- A `stroke` takes no room from the layout, unlike a CSS border: draw it as a box shadow (`inset 0 0 0 1px` for `inside`, a spread for `outside`) or an outline.
- `lineHeight` is a unitless line height; `letterSpacing` is in px.
- A shadow's `blur` is CSS's blur value; a `blur` or `background_blur` effect's `radius` is twice CSS's (`backdrop-filter: blur(radius / 2)`).
- An icon is the Iconify icon `set:name` at the node's size, in its fill colour.

## 4. Compare

Screenshot what you're implementing, with the node as small as shows it, and compare with your page. Check the behaviour the Design implies: fill grows with its container, fit shrinks to its content, nothing overflows sideways at the design width, the console is clean.

```js
TakeScreenshot([button, screen]);
```

## Exports

`Export` writes files into the project for you to start from or deliver:

- PNG of any node, for assets or comparison: `Export([id], {format: "png", path, scale: 2})`.
- HTML of Screens, with plain CSS or Tailwind: `Export([screens], {format: "html", path: "file.html", css: "css"})`. Auto layout becomes flexbox, Tokens become custom properties, shapes and icons inline SVG. It's a reference for structure and values, not code to ship: rebuild it in the project's components.

A relative `path` is from the Design's folder; give an absolute path into the project to write there.

```js
const written = Export([screen], { format: "html", path: "handoff/settings.html", css: "css", layerNames: true });
Print(written);
Export([button], { format: "png", path: "handoff/assets", scale: 2 });
```
