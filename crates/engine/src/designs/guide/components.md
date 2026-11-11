# Components

A **Component** is a Frame with `reusable: true`. `Copy` of it makes an **Instance**, which draws the Component and follows every later change to it, with its own overrides on top. Use Components for anything that repeats: buttons, fields, toggles, cards, nav items, list rows, metrics.

## When and how to make one

- Make a Component as soon as a widget appears twice. Build it once, check it, then place Instances.
- Make one generic Component that Instances adapt (a label, an icon, a colour), not near-duplicates. Make states that change the structure (a button with and without an icon, a filled and an empty field) separate Components.
- Name variants with slashes: `Button/Primary`, `Button/Secondary`, `Field/Error`. The name says what it is, not what it looks like.
- Give the Component's inner nodes clear, unique names (`Label`, `Icon`), since Instances override them by name.
- Size Components by their content (`fit_content`) or to fill their parent, so Instances adapt to where they're placed. Fixed pixel widths are for things that really are fixed (an avatar, an icon button).
- Keep Components together on the Canvas, away from the Screens, and placed with `FindEmptySpace`.
- Build from Tokens, so a Theme change reaches every Instance.

```js
SetTokens({
  "primary": { type: "color", value: "#2563EB" },
  "on-primary": { type: "color", value: "#FFFFFF" },
  "surface": { type: "color", value: "#FFFFFF" },
  "border": { type: "color", value: "#E4E4E7" },
  "fg": { type: "color", value: "#18181B" },
  "muted": { type: "color", value: "#71717A" },
  "radius-pill": { type: "number", value: 999 },
  "radius-md": { type: "number", value: 10 },
});
const label = { type: "text", name: "Label", fontSize: 14, fontWeight: 600 };
primary = Insert({ type: "frame", name: "Button/Primary", reusable: true, x: 0, y: 0, layout: "horizontal", gap: 8,
  padding: [10, 16], alignItems: "center", cornerRadius: "$radius-pill", fill: "$primary", children: [
    { ...label, content: "Continue", fill: "$on-primary" },
    { type: "icon", name: "Icon", icon: "lucide:arrow-right", width: 16, height: 16, fill: "$on-primary" },
  ] });
const spot = FindEmptySpace({ width: 120, height: 40, from: primary, padding: 24 });
secondary = Insert({ type: "frame", name: "Button/Secondary", reusable: true, ...spot, layout: "horizontal", gap: 8,
  padding: [10, 16], alignItems: "center", cornerRadius: "$radius-pill", fill: "$surface",
  stroke: { fill: "$border", thickness: 1 }, children: [{ ...label, content: "Cancel", fill: "$fg" }] });
TakeScreenshot([primary, secondary]);
```

## Placing and customising Instances

- Override the Instance's own properties directly in the Copy (`fill`, `width`, `x`).
- Override nodes inside it with `descendants`, keyed by the inner node's id in the Component, by its name when only one node has it, or by a slash path through a nested Instance (`buttonId/labelId`).
- A descendant given properties has them set; given a whole node with a `type`, it's replaced by that node.
- Hide an inner node with `enabled: false`. You can't Insert into, Move or Delete nodes inside an Instance.
- Set overrides as you Copy: the new Instance's ids aren't known until the call returns.
- Later, reach an inner node with `instanceId/nodeId` in `Get`, `Update` and `Replace`.

```js
const screen = Insert({ type: "frame", name: "Dialog", ...FindEmptySpace({ width: 400, height: 200 }), width: 400,
  layout: "vertical", gap: 24, padding: 24, cornerRadius: 16, fill: "$surface", stroke: { fill: "$border", thickness: 1 } });
Insert(screen, { type: "text", name: "Question", content: "Archive this project?", fontSize: 18, fontWeight: 600, fill: "$fg", width: "fill_container" });
actions = Insert(screen, { type: "frame", name: "Actions", width: "fill_container", layout: "horizontal", gap: 12, justifyContent: "end" });
Copy(secondary, actions, {});
archive = Copy(primary, actions, { fill: "#DC2626", descendants: { Label: { content: "Archive" }, Icon: { enabled: false } } });
TakeScreenshot(screen);
```

```js
const inner = Get(archive, { expandInstances: true, depth: 1 });
Print(inner.children.map((child) => `${child.id} ${child.name} ${child.enabled === false ? "hidden" : ""}`));
const labelId = Get(primary).children[0].id;
Update(`${archive}/${labelId}`, { content: "Archive project" });
```

## Nested Components

A Component can hold Instances of other Components. Reach through them with slash paths, both in `descendants` and in edits.

```js
const buttonLabel = Get(primary).children[0].id;
card = Insert({ type: "frame", name: "Card/Plan", reusable: true, ...FindEmptySpace({ width: 280, height: 200, from: primary, direction: "bottom" }),
  width: 280, layout: "vertical", gap: 12, padding: 24, cornerRadius: "$radius-md", fill: "$surface", stroke: { fill: "$border", thickness: 1 },
  children: [
    { type: "text", name: "Plan", content: "Starter", fontSize: 14, fontWeight: 600, fill: "$muted" },
    { type: "text", name: "Price", content: "$12", fontSize: 32, fontWeight: 600, fill: "$fg" },
  ] });
cta = Copy(primary, card, { width: "fill_container", justifyContent: "center" });
const pro = Copy(card, null, { ...FindEmptySpace({ width: 280, height: 200, from: card, padding: 40 }),
  descendants: { Plan: { content: "Pro" }, Price: { content: "$29" }, [`${cta}/${buttonLabel}`]: { content: "Go Pro" } } });
TakeScreenshot([card, pro]);
```

## Changing Components

- Change the Component, never each Instance, when the change should reach everywhere: every Instance follows, under its overrides.
- Read every Instance's overrides (`Get` on the Instance shows its `descendants`) before restructuring a Component: renaming or deleting an inner node orphans overrides that name it.
- To detach one Instance from its Component, Replace it with a plain Frame copied from what `Get(id, {expandInstances: true, depth: Infinity})` read.

```js
Update(primary, { padding: [12, 20] });
TakeScreenshot([primary, card]);
```
