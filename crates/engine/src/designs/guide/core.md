# Designing in Zeron: the core guide

You design by writing small JavaScript snippets that `design_execute` runs against a **Design**: one `.zdesign` file holding an infinite **Canvas** of **Screens** (top-level Frames, each one web view, app screen or slide), **Components**, and the **Tokens** and **Themes** they draw from. The person may be watching the Design in a Design tab beside the chat as you work, and may select nodes or edit them by hand. Your result returns the ids of what you made, lines you printed, warnings, and screenshots you asked for.

You can't open a Design tab, and nothing opens one on its own. To show the person a Design, tell them to open the Design row (or press D) in the chat's panel, where it's under Recent designs.

`design_state` also says what the person has selected in each open Design (`{id, name, type}`; inside an Instance, `instance/node`). When they say "this" or "make it bigger", call `design_state` first and act on those ids. The person may move, retype and restyle nodes by hand while you work, and undo either of you: if your result says the Design changed since your last call, read it again before you change what you didn't just make.

This guide is the rules, the API and the format, how to check your work, and how to design well. Topic guides go deeper: call `design_guide` with `components`, `design-systems`, `landing-pages`, `mobile-apps`, `web-apps`, `slides`, `tables`, `code` (implementing a Design) or `tailwind`. Load the ones your task touches before you start.

## The working loop

1. **Orient.** `design_state` lists the Designs open in your chat's Design tabs (the front one first) and recent ones. For an existing Design, read before you write: `Get()` the Canvas, `GetTokens()`, and `Get(id, {depth: Infinity})` the Screen you'll touch.
2. **Settle the content and the direction.** Write the real words first; the content decides the layout. Then commit to a direction (see *Direction*) and store it as Tokens.
3. **Build in passes, one section per call.** Tokens, then Components, then Screens made of Instances. Each call builds one section (a header, a hero, a table), checks it with a Get visitor, and ends with a screenshot of the smallest node that shows it.
4. **Fix what the result tells you** (warnings, problems, what the screenshot shows) in the next call, by updating the nodes you have, not by deleting and rebuilding.
5. **Finish**: clear `inProgress`, check the whole Screen once, and tell the person what you made.

Do what was asked and only that: a request to recolour, rename or move gets exactly that change.

## Calls

`design_execute` takes:

- `snippet`: the JavaScript.
- `description`: a few words; it labels the call's undo step, so give one every time.
- Which Design: `path` (a `.zdesign` file anywhere on this computer, from a previous result's `design.path`), or `name` (makes a new Design in Zeron's designs folder), or neither (the Design in your chat's front Design tab; with no tab open the call fails and lists the open and recent Designs).

A successful call is one undo step and is applied at once. A managed Design (in Zeron's designs folder) saves itself within a few seconds; a `.zdesign` opened from elsewhere changes on disk only when the person saves it, and your result says `unsaved` meanwhile.

The result:

- `design`: `{name, path}`. Keep `path` for later calls.
- `ids`: a map from each new node's `name` to its id. A name used twice in one call is listed as `Name 2`, `Name 3`.
- `printed`: what `Print` wrote.
- `warnings`: `{node, property?, message}` for things that didn't fail the call but need fixing: an unknown property (kept, but it does nothing; usually a typo), text without a fill, a font or image that can't be had. Fix every one in your next call.
- `screenshots`: one image per id you passed to `TakeScreenshot`.
- `exported`: paths `Export` wrote.
- `notice`: someone else (another chat, or the person) changed the Design since your last call. Re-read the nodes you're about to touch before changing them.

**When a call fails, nothing changes.** You get `{error, line, code, editId}`. Fix it by calling `design_execute` again with only `editId` and `edits`, find-and-replace edits to the failed snippet, each `find` being text that appears in it once:

```json
{ "editId": "e3k9", "edits": [{ "find": "fil: \"#FFF\"", "replace": "fill: \"#FFFFFF\"" }] }
```

Send no snippet, path or name with a retry. Retry as often as it takes.

**Limits.** A snippet runs for at most 5 seconds (time waiting on the network for icons, fonts and images doesn't count, up to 30 s per call), in 64 MB. It has no files, network or modules: only the API below and plain JavaScript.

**Globals.** Snippets run non-strict, and a name you assign without declaring it (`hero = Insert(...)`) is a global carried to your chat's next call. Use that to keep ids between calls. Globals carry only JSON-able values (no functions), up to 1 MB per chat, only from calls that succeed, and are lost when Zeron restarts, so re-read ids with `Get` if one comes back undefined. Declare everything else with `const` or `let`.

## The API

Every function takes and returns plain values; ids are strings.

### Insert(parent, node) → id

Adds `node` as the last child of `parent`; `Insert(node)` puts it on the Canvas. A node is an object of properties (see *Nodes*), and a Frame takes its `children` inline, so one Insert can build a whole tree. Every node gets a fresh id; give every node a readable `name`, which is how `ids` reports it and how you and the person find it.

```js
screen = Insert({ type: "frame", name: "Home", x: 0, y: 0, width: 1440, height: 900, fill: "#FAFAF9",
  layout: "vertical", padding: [48, 64], gap: 32, clip: true, inProgress: true });
header = Insert(screen, { type: "frame", name: "Header", width: "fill_container", layout: "horizontal",
  justifyContent: "space_between", alignItems: "center", children: [
    { type: "text", name: "Wordmark", content: "Northwind", fontSize: 22, fontWeight: 700, fill: "#1C1917" },
    { type: "frame", name: "Nav", layout: "horizontal", gap: 24, children: ["Product", "Pricing", "Docs"].map((label) =>
      ({ type: "text", name: label, content: label, fontSize: 15, fill: "#57534E" })) },
  ] });
```

Generate repeated structure with arrays, `map`, loops and small helper functions, and spread shared styles (`{...body, content}`).

### Get(id, options) → node

Reads a node as the file holds it, so what you read can be changed and sent back. It reads one level of children by default; pass `{depth: n}` for more, or `{depth: Infinity}` for all. `Get()` reads the Canvas's nodes as a list. Options `resolveTokens: true` gives Token references their values, and `expandInstances: true` reads each Instance as the nodes it draws.

```js
const home = Get(screen, { depth: 2 });
Print(home.children.map((child) => child.name));
```

### Get(id, visitor, options): walk with layout

With a function, `Get` visits the node and all its descendants, parents first, calling `visitor(node, info)` with the node (without its children) and `info`:

- `bounds`: `{x, y, width, height}` on the Canvas, as laid out and drawn.
- `depth`: levels below where the visit started.
- `index`, `parent`: its place among its siblings.
- `problems`: layout problems. `partially clipped` and `fully clipped` (a clipping Frame cuts it off), `text taller than its height, overflowing it`, and `fit content around children that all fill container: …` (see *Sizing*).

Return `false` from the visitor to skip a node's children. With no id it visits the whole Canvas. This is how you check layout: it's exact, and it costs far less than a screenshot.

```js
Get(screen, (node, info) => {
  const { x, y, width, height } = info.bounds;
  Print(`${"  ".repeat(info.depth)}${node.name} ${x},${y} ${width}x${height} ${info.problems.join("; ")}`);
});
```

### Update(id, props)

Sets properties; `null` unsets one. It can't change a node's `id`, `type`, `ref` or `children` (use Insert, Move, Delete and Replace for those), but you can Update a node with what `Get` read, changed.

```js
Update(screen, { fill: "#F5F5F4" });
const nav = Get(header).children[1];
Update(nav.id, { ...nav, gap: 32 });
```

### Copy(id, parent, props) → id

Copies a node and its descendants with new ids into `parent` (`null` for the Canvas), setting `props` on the copy. `props.descendants` sets properties on nodes inside the copy, keyed by their ids in the original; set them during the Copy, as the copy's ids are new. Copying a Component makes an **Instance** of it (see *Components and Instances*).

```js
const second = Copy(header, screen, { name: "Footer" });
Update(second, { justifyContent: "start", gap: 48 });
```

### Replace(id, node) → id

Puts a new node (as Insert takes it) where `id` is among its siblings, deleting `id` and its descendants.

### Move(id, parent, index)

Moves a node into `parent` (`null` for the Canvas) at `index` among its children, or last without one. It keeps its `x` and `y`, now from the new parent.

### Delete(id) or Delete([ids])

Deletes nodes with their descendants: all of them, or none when one isn't there.

```js
const note = Insert(screen, { type: "text", name: "Draft note", content: "temp", fill: "#000000" });
Move(note, header, 0);
const swapped = Replace(note, { type: "text", name: "Tagline", content: "Fresh every week", fontSize: 13, fill: "#78716C" });
Delete(swapped);
```

### FindEmptySpace({width, height, direction, padding, from}) → {x, y}

A free spot on the Canvas for a new top-level node: beside all the Canvas's nodes, or beside the node `from`, toward `direction` (`right`, the default, `left`, `top` or `bottom`), keeping `padding` (100 by default) from everything. Place every new Screen with it rather than with guessed coordinates; chain each from the previous Screen so they read in order.

```js
const spot = FindEmptySpace({ width: 390, height: 844, from: screen, padding: 120 });
mobile = Insert({ type: "frame", name: "Home (mobile)", ...spot, width: 390, height: 844, fill: "#FAFAF9", clip: true });
```

### TakeScreenshot(id) or TakeScreenshot([ids])

Attaches a PNG of each node to the call's result, drawn at 1x and scaled down so its longest side is at most 400 px. A 1440-wide Screen comes back at about a quarter size, where body text is unreadable, so screenshot the smallest node that shows what you need: a section, a card, a button. Use screenshots for colour, type and alignment; use a Get visitor for sizes, positions and clipping. A screenshot shows a rotated node upright.

### Print(...values)

Adds one line to the result's `printed`: strings as they are, anything else as JSON. Print compact facts you asked for, not whole trees.

### SetTokens(tokens, {replace}) and GetTokens()

See *Tokens and Themes*. `GetTokens({css: true})` returns the Tokens as CSS custom properties.

### Export(ids, options) → paths

Writes files for whoever implements the Design; it's for delivery, not for checking your work. A relative `path` is from the Design's folder.

- `Export([ids], {format: "png", path: folder, scale})`: each node as a PNG in `folder`, named after the node, at `scale` (2 by default, at most 8).
- `Export([screens], {format: "html", path: "file.html", css, scaffold, layerNames})`: Screens (every Screen with no ids) as one HTML file. `css` is `"css"` (plain, the default) or `"tailwind"`; `scaffold: false` writes just the Screens and their styles instead of a whole page; `layerNames: true` puts each node's name on its element as `data-name`. Auto layout becomes flexbox; it's a starting point to adapt, not production code.

```js
Export([header], { format: "png", path: "exports", scale: 2 });
Export([screen], { format: "html", path: "exports/home.html", css: "css" });
```

## Nodes

A node is `{type, name, ...properties}`. Unknown properties are kept but do nothing, with a warning, so a warning about one is almost always a typo. Nodes nest at most 48 deep.

**Every node:** `id` (given by Insert), `name`, `x`, `y` (from its parent's top left), `rotation` (degrees counter-clockwise about its top left; it turns its children too), `opacity` (0 to 1), `enabled` (`false` hides it and leaves it out of layout), `flipX`, `flipY`, `absolute` (placed at its `x`/`y` even inside a layout, and left out of it), `theme` (see *Tokens*), `inProgress`.

**Types:**

- `frame`: a container. `width`, `height`, layout (below), `fill`, `stroke`, `effect`, `cornerRadius`, `clip` (cut children off at its bounds), `reusable` (makes it a Component), `children`. A Frame without a layout is also how you group things.
- `rectangle`: `width`, `height`, `cornerRadius`, `fill`, `stroke`, `effect`.
- `ellipse`: `width`, `height`, `fill`, `stroke`, `effect`, and for arcs and rings `startAngle` (degrees, 0 at 3 o'clock, clockwise), `sweepAngle` (360 by default) and `innerRadius` (0 to 1, a fraction of its radii).
- `polygon`: a regular polygon with `polygonCount` sides (3 to 64), one corner at the top, stretched to its box.
- `path`: SVG path data in `geometry`, with `viewBox: [x, y, width, height]` stretched onto its box (without one, the geometry's own bounds are), and `fillRule` (`nonzero` or `evenodd`).
- `text`: see *Text*.
- `icon`: `icon: "set:name"` from any Iconify set (`lucide:search`, `ph:house-fill`, `material-symbols:settings-outline`), drawn in its first solid `fill` colour at `width` × `height`; `weight` (100 to 900, 400 the set's regular) thins or thickens icons drawn with strokes (Lucide, Tabler) and picks Phosphor's weights, and warns on other sets. An icon named directly that doesn't exist fails the call; one named through a Token warns and draws nothing. Use icons rather than drawing pictograms with paths.
- `instance`: made by copying a Component, `{ref, descendants, ...overrides}`.

`cornerRadius` is one number or four `[topLeft, topRight, bottomRight, bottomLeft]`.

```js
Insert(screen, { type: "frame", name: "Shapes", layout: "horizontal", gap: 16, alignItems: "center", children: [
  { type: "rectangle", name: "Tile", width: 48, height: 48, cornerRadius: [12, 12, 0, 12], fill: "#E7E5E4" },
  { type: "ellipse", name: "Ring", width: 48, height: 48, innerRadius: 0.7, startAngle: -90, sweepAngle: 270, fill: "#0F766E" },
  { type: "polygon", name: "Hexagon", width: 48, height: 48, polygonCount: 6, fill: "#F59E0B" },
  { type: "path", name: "Check", width: 24, height: 24, viewBox: [0, 0, 24, 24], geometry: "M4 12 L10 18 L20 6",
    stroke: { fill: "#1C1917", thickness: 2.5, cap: "round", join: "round" } },
  { type: "icon", name: "Search", icon: "lucide:search", width: 24, height: 24, fill: "#1C1917", weight: 300 },
] });
```

### Layout and sizing

A Frame lays its children out with `layout: "horizontal"` (a row) or `"vertical"` (a column); without it (`"none"`) children sit at their own `x` and `y`. In a layout, children's `x` and `y` are ignored unless they're `absolute`.

- `gap`: space between children.
- `padding`: one number, `[vertical, horizontal]`, or `[top, right, bottom, left]`.
- `justifyContent`: along the layout: `start`, `center`, `end`, `space_between`, `space_around`.
- `alignItems`: across it: `start`, `center`, `end`.

Each of `width` and `height` is a number of pixels, `"fit_content"` (as big as its children need, a Frame's default) or `"fill_container"` (the room its parent leaves; several fill children share it equally).

What isn't there, and what to do instead:

- No percentages, viewport units, `calc` or min/max sizes: use fill and fit, and fixed numbers.
- No margins, and no padding on text: wrap the child in a Frame with padding.
- No wrapping: build grids as a column of rows.
- No stretch or baseline alignment: give children `width: "fill_container"` to span a column.
- Nothing scrolls: everything you design stays visible inside its Screen, so let Screens grow (`height: "fit_content"`) or give them room.

**The one sizing trap.** A fit-content parent whose children all fill it has nothing to measure: each empty filling child counts as 100 px and Get reports `fit content around children that all fill container`. Give the parent a real width (or let it fill its own parent), or give a child a fixed size.

### Paint

`fill` is one paint or a list drawn in order (the last on top):

- A hex string: `"#RGB"`, `"#RRGGBB"` or `"#RRGGBBAA"`.
- `{type: "color", color, opacity, blendMode, enabled}`.
- `{type: "gradient", gradientType: "linear" | "radial" | "angular", colors: [{color, position}], center: {x, y}, size: {width, height}, rotation}`. Geometry is relative to the node's bounds (`center` 0 to 1, `size` 1 = the side). At `rotation: 0` a linear gradient runs upward (bottom colour first, top colour last); rotation is counter-clockwise. `colors` can also be plain hex strings, spaced evenly.
- `{type: "image", url, mode: "cover" | "contain" | "stretch"}`. `url` is a path relative to the Design's folder (copy files into its `images/` folder: `images/photo.png`) or an http(s) URL, downloaded once and cached. Absolute paths, `..` and `data:` URLs are refused. A missing image draws as a grey crossed box, with a warning. Images are fills only.

`blendMode` is `normal`, `darken`, `multiply`, `colorBurn`, `lighten`, `screen`, `colorDodge`, `linearDodge`, `overlay`, `softLight`, `hardLight`, `difference`, `exclusion`, `hue`, `saturation`, `color` or `luminosity`.

`stroke` is `{fill, thickness, align, join, cap, dashPattern}`: `fill` as above; `thickness` a number or `{top, right, bottom, left}`; `align` `inside` (the default), `center` or `outside`; `join` `miter`, `round` or `bevel`; `cap` `none`, `round` or `square`; `dashPattern` a list of dash and gap lengths. Per-side thicknesses draw solid, mitered lines.

`effect` is one effect or a list:

- `{type: "shadow", shadowType: "outer" | "inner", offset: {x, y}, blur, spread, color}` (a blur radius, as in CSS).
- `{type: "blur", radius}` blurs the node itself.
- `{type: "background_blur", radius}` blurs what's behind it, for frosted panels over a translucent fill.

```js
Insert(screen, { type: "frame", name: "Card", width: 320, height: 180, cornerRadius: 16,
  fill: [{ type: "gradient", gradientType: "linear", rotation: -45, colors: [{ color: "#0F766E", position: 0 }, { color: "#134E4A", position: 1 }] }],
  stroke: { fill: "#FFFFFF33", thickness: 1, align: "inside" },
  effect: [{ type: "shadow", offset: { x: 0, y: 8 }, blur: 24, spread: -4, color: "#1C191729" }] });
```

### Text

`{type: "text", content, fill, fontFamily, fontSize, fontWeight, lineHeight, letterSpacing, textAlign, textAlignVertical, textGrowth, underline, strikethrough}`.

- **Every text needs a `fill`**, or it's invisible.
- `fontFamily`: any Google Fonts family by its exact name, or a font file in a `fonts/` folder beside the `.zdesign`. Geist is the default. Characters a family lacks (emoji, other scripts) fall back to Geist, then Noto.
- `fontSize` in px (14 by default); `fontWeight` 100 to 900; `lineHeight` a multiple of the font size (`1.5`); `letterSpacing` in px (negative tightens).
- `textAlign`: `left`, `center`, `right`, `justify`. `textAlignVertical`: `top`, `middle`, `bottom`.
- `textGrowth`: `auto` (one line per paragraph, as wide as its text), `fixed_width` (wraps at its width and grows in height) or `fixed_width_height` (a fixed box; text past it overflows). Unset, it follows what you set: a width means `fixed_width`, a width and height `fixed_width_height`.

Paragraphs need a width to wrap: in a layout, `width: "fill_container"`; elsewhere a number. Never guess a text box's height; let it grow. Get reports text taller than a fixed height.

```js
Insert(screen, { type: "frame", name: "Intro", width: 560, layout: "vertical", gap: 16, children: [
  { type: "text", name: "Title", content: "Groceries, planned for you", fontFamily: "Instrument Serif", fontSize: 56, lineHeight: 1.05, letterSpacing: -1, fill: "#1C1917", width: "fill_container" },
  { type: "text", name: "Body", content: "Tell us what you like and we will plan a week of meals, with one shopping list.", fontSize: 18, lineHeight: 1.5, fill: "#57534E", width: "fill_container" },
] });
```

### Tokens and Themes

A **Token** is a named `color`, `number` or `string` that any property refers to as `"$name"` instead of a literal. Name Tokens without the `$` (letters, digits, `-`, `_`, `.`, `/`). A Token can vary by **Theme**: give it a list of `{value, theme: {axis: value}}`, and the axes and values join the Design's Themes. A node shows the first value of each axis unless it, or a node above it, sets `theme: {mode: "dark"}`.

`SetTokens` merges into the existing Tokens (`null` removes one); `{replace: true}` replaces them all, Themes too. `GetTokens()` returns `{tokens, themes}`. Read the existing Tokens before writing: they're binding, so extend them rather than overwrite them.

```js
SetTokens({
  "bg": { type: "color", value: [{ value: "#FAFAF9", theme: { mode: "light" } }, { value: "#0C0A09", theme: { mode: "dark" } }] },
  "fg": { type: "color", value: [{ value: "#1C1917", theme: { mode: "light" } }, { value: "#FAFAF9", theme: { mode: "dark" } }] },
  "accent": { type: "color", value: "#0F766E" },
  "radius-md": { type: "number", value: 12 },
  "font-display": { type: "string", value: "Instrument Serif" },
});
const tokens = GetTokens();
Print(Object.keys(tokens.tokens), tokens.themes);
const swatch = Insert(screen, { type: "frame", name: "Swatch", width: 200, height: 80, padding: 16, cornerRadius: "$radius-md", fill: "$bg",
  children: [{ type: "text", name: "Swatch label", content: "Aa", fontFamily: "$font-display", fontSize: 32, fill: "$fg" }] });
const dark = Copy(swatch, screen, { name: "Swatch (dark)", theme: { mode: "dark" } });
TakeScreenshot([swatch, dark]);
```

### Components and Instances

`reusable: true` makes a Frame a **Component**. `Copy` of a Component makes an **Instance** that follows it: a change to the Component reaches every Instance. An Instance overrides the Component's own properties directly (`fill`, `width`), and the nodes inside it through `descendants`, keyed by the node's id in the Component, a slash path into a nested Instance (`button/label`), or a name only one node has. A descendant given an object of properties has them set; given a full node (with a `type`), it's replaced.

Reach a node inside an Instance with a slash path, `instanceId/nodeId`, in `Get`, `Update` and `Replace`. You can't Insert into, Move or Delete nodes inside an Instance: hide one with `enabled: false`.

```js
button = Insert({ type: "frame", name: "Button", reusable: true, x: 0, y: -200, layout: "horizontal", gap: 8, padding: [10, 16],
  alignItems: "center", cornerRadius: 999, fill: "$accent", children: [
    { type: "text", name: "Label", content: "Get started", fontSize: 15, fontWeight: 600, fill: "#FFFFFF" },
    { type: "icon", name: "Arrow", icon: "lucide:arrow-right", width: 16, height: 16, fill: "#FFFFFF" },
  ] });
const primary = Copy(button, header, {});
const quiet = Copy(button, header, { fill: "#E7E5E4", descendants: { Label: { content: "Sign in", fill: "#1C1917" }, Arrow: { enabled: false } } });
const label = Get(button).children[0].id;
Update(`${primary}/${label}`, { content: "Start free" });
TakeScreenshot(header);
```

See the `components` and `design-systems` topic guides for more.

## Checking your work

Check each section as soon as it's built, in the call that builds it, and fix what you find before moving on.

1. **Read the layout.** Walk the section with a Get visitor and print one line per node: its name, bounds and problems. Everything you meant to see should have a size, sit inside its parent, and have no problems.
2. **Look at it.** Screenshot the section (not the whole Screen) to judge colour, type, contrast, alignment and balance.
3. **Read the result.** Fix every warning. A `notice` means re-read before changing anything else.

Checks worth scripting as visitors:

```js
const issues = [];
Get(screen, (node, info) => {
  if (info.problems.length) issues.push(`${node.name}: ${info.problems.join("; ")}`);
  if (node.type === "text" && (node.fontSize ?? 14) < 12) issues.push(`${node.name}: text below 12 px`);
  if (info.depth === 1 && info.bounds.width === 0) issues.push(`${node.name}: collapsed`);
});
Print(issues.length ? issues : "no problems");
```

Before calling a Screen done: nothing clipped (grow the container rather than shrink content); nothing collapsed to zero; text readable, with enough contrast; spacing from your scale; edges aligned; what rendered is what you meant.

```js
Update(screen, { inProgress: false });
TakeScreenshot(header);
```

## The Canvas

- Keep the top level for Screens, Components and a few large frames, never loose text, icons or buttons.
- Components sit together above or beside the Screens; Screens run left to right (and down for variants), each placed with `FindEmptySpace`.
- Each Screen is one Frame with `clip: true`, at the real size of its device: 1440 wide for desktop web, 390 for a phone, 1920 × 1080 for a slide. Let its height grow with its content unless the format fixes it.
- Set `inProgress: true` on a Screen while you build it and clear it the moment it's done, so the person can tell finished work from work in progress.
- Every change you make flashes in the person's Design tab. Make a few deliberate calls, one per section, rather than many tiny ones.
- The person may edit the Design while you work. When something isn't where you left it, re-read it; keep their changes rather than overwriting them.

## Designing well

### Hierarchy

- Each Screen has one job: one main question it answers and one main action. When two goals compete, split them across Screens.
- Each region has one focal point. Rank with size first, then weight, then colour. If everything has equal weight, nothing leads.
- Actions are ranked: one filled primary per section; alternatives step down to secondary, outlined, then text styles. Destructive actions get their own danger style; rarely used ones go into a menu.
- The top of a Screen tells the reader where they are and what they can do here.

### Spacing

- Choose a 4- or 8-px scale and use only its values. Repeat the same rhythm between sections.
- Proximity groups: keep related items close and put more space between groups. Spacing separates better than lines; add a divider only when it carries meaning.
- Starting points for app UI: 24–32 between major sections; 16–24 between cards; 16 between form fields; 12 in a button group; 24 inside cards; buttons about 10 × 16 padding; inputs about 8 × 16; content areas 32.
- Pick one density per Screen (compact for data-heavy tools, comfortable by default, airy for simple flows) and keep it.

### Type

- One or two families, held in Tokens (a display face and a text face).
- Size steps between levels big enough to see at a glance; within a level, vary weight rather than size.
- Body text at least 14 px on the web (16 better), lines about 50–75 characters, centred text no more than three lines, capitals only for small labels.
- Tighten letter spacing slightly on large display type and loosen line height on body text.

### Colour

- A small palette: one accent, a neutral ramp, and semantic colours (success, warning, error, info), each as a background and foreground pair.
- The accent marks actions and emphasis only. Body text stays neutral, with strong contrast on every surface.
- Put every colour in a Token before using it.

### Restraint

- A container earns its place by doing a structural job. Putting every element in a card, with a border and a shadow, is the quickest way to look generic.
- Use gradients, shadows and large radii sparingly, unless the direction is built on them.
- Avoid the opposite failure too: a page of identical grids reads as machine-made. Give each Screen at least one distinctive move: an oversized number, an asymmetric split, a full-bleed band, a typographic headline.
- Every element should help the reader navigate, understand, decide or act. Cut what doesn't.
- Make marks from type, icons and simple shapes: a wordmark in the display face, a geometric mark. Leave logos, mascots and illustrations to images the person provides.

### Without images

You can't generate images. Use images the person gives you (copy them into the Design's `images/` folder) or URLs they point to. Otherwise design type-led and colour-block layouts, or reserve an image area: a Frame in a quiet fill with a small label saying what goes there. Text over an image always gets a scrim or a solid band behind it.

### Charts

Build bar charts from Frames in a layout, and donuts and gauges from ellipses with `innerRadius`, `startAngle` and `sweepAngle`. Prefer bars to lines. Put labels in the layout beside the chart, not floating over it.

## Direction

### Working from references

When the person gives you references (screenshots, URLs, a brand guide, an existing Design or codebase), they decide the direction. Before building, write down what they establish: typefaces, palette, radii, density, spacing, how they use imagery. Then match it. Adapt a brand guide to the format rather than copying it pixel for pixel (slides scale type up; mobile tightens spacing). In an existing Design:

- Read its Tokens and Components first, and build from them. Its Tokens are binding.
- Copy an existing Screen or section and edit the copy rather than starting from nothing.
- Frames that look like components in someone else's Design are their components: reuse them, or turn them into Components.

### When none is given

Commit to one coherent direction before the first Screen, and hold it everywhere. Write it down in a sentence or two, then store it as Tokens:

- **Mood** in a few words: calm and editorial; precise and technical; warm and playful.
- **Type pairing**: a display face and a text face (Instrument Serif with Geist; Geist alone with Geist Mono for numbers).
- **Palette**: one accent, a neutral ramp tinted toward it (warm greys with a teal accent, cool greys with a blue one), semantic colours.
- **Radius scale** and **density**.

Some directions that work: an editorial serif with generous margins; a technical blueprint with a monospace face, hairline strokes and a grid; bento grids of varied tiles; cinematic full-bleed bands; stacked colour blocks; a dark platform look with one luminous accent. Choose one that fits the product and the audience, and resist mixing two.

Then build only from the Tokens, so the direction holds across every Screen.
