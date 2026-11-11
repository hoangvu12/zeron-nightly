# Design systems

A design system here is a Design's Tokens and Themes plus a set of Components built only from them, laid out on the Canvas so a person (or another chat) can read it. Build it before the Screens that use it, or extract it from Screens that already exist.

## Order

1. Read what's there: `GetTokens()` and the Components on the Canvas. Existing Tokens are binding; extend them, and replace them (`{replace: true}`) only when asked to.
2. Write the Tokens: colours with a light and dark value, then type, radius and spacing.
3. Build the core Components from the Tokens, one family per call, each checked with a screenshot.
4. Lay them out on the Canvas in labelled rows, so the system reads as a reference sheet.

## Tokens

Name Tokens by role, not by colour: `primary`, not `blue-600`. A useful set:

- Surfaces: `background`, `card`, `popover`.
- Text: `foreground`, `muted-foreground`.
- Lines: `border`, `input`, `ring` (focus).
- Actions: `primary`, `primary-foreground`, `secondary`, `secondary-foreground`, `destructive`, `destructive-foreground`.
- Status, each a background and a foreground: `success`, `warning`, `error`, `info`.
- Type: `font-primary` (headings, labels, navigation) and `font-secondary` (body, inputs); sizes as numbers if the scale matters.
- Radius: none for tables and dense data; medium for cards and dialogs; a pill for buttons, inputs and badges.
- Spacing: the scale's steps, when you'll use them as numbers (`space-4: 16`).

Give a colour Token one value per Theme (`[{value, theme: {mode: "light"}}, {value, theme: {mode: "dark"}}]`); numbers and strings usually don't vary. The first value of each axis is what nodes show unless a node sets `theme`.

```js
const themed = (light, dark) => ({ type: "color", value: [{ value: light, theme: { mode: "light" } }, { value: dark, theme: { mode: "dark" } }] });
SetTokens({
  "background": themed("#FFFFFF", "#09090B"),
  "card": themed("#FFFFFF", "#18181B"),
  "foreground": themed("#09090B", "#FAFAFA"),
  "muted-foreground": themed("#71717A", "#A1A1AA"),
  "border": themed("#E4E4E7", "#27272A"),
  "primary": themed("#18181B", "#FAFAFA"),
  "primary-foreground": themed("#FAFAFA", "#18181B"),
  "info": themed("#EFF6FF", "#172554"),
  "info-foreground": themed("#1D4ED8", "#BFDBFE"),
  "font-primary": { type: "string", value: "Geist" },
  "font-secondary": { type: "string", value: "Geist" },
  "radius-md": { type: "number", value: 8 },
  "radius-pill": { type: "number", value: 999 },
});
Print(GetTokens().themes);
```

## The core set

Most product UI is built from: button variants (primary, secondary, outline, ghost, destructive), an input with its label and help text, a card, a sidebar with item states, tabs, a dropdown or list item, a table row and cell, pagination, an alert, and a dialog. Build what the product needs from this list, not all of it.

Recipes that hold up:

- **Card**: a header (title and one line of description), the content, then actions aligned right.
- **Sidebar**: a brand header, items grouped under small section labels with one item active, the user at the bottom.
- **Tabs**: a row of labels with the first active, marked by weight and a line or a filled pill.
- **Dropdown**: an optional search, items with a check on the chosen one, dividers between groups, small group titles.
- **Pagination**: previous and next, a few page numbers, an ellipsis for the gap.
- **Form**: short fields side by side (first and last name), then full-width fields.
- **Metric**: a muted label over a large value (about 32, semibold), then a small change indicator.
- **Alert**: an icon, a title and one sentence, in a status colour pair.

Where actions go: bottom right in cards, dialogs and forms; in toolbars, the primary on the left and the rest on the right; in a destructive dialog, Cancel then the destructive action, on the right.

```js
alert = Insert({ type: "frame", name: "Alert/Info", reusable: true, x: 0, y: 0, width: 420, layout: "horizontal", gap: 12,
  padding: 16, cornerRadius: "$radius-md", fill: "$info", children: [
    { type: "icon", name: "Icon", icon: "lucide:info", width: 20, height: 20, fill: "$info-foreground" },
    { type: "frame", name: "Text", width: "fill_container", layout: "vertical", gap: 4, children: [
      { type: "text", name: "Title", content: "Heads up", fontFamily: "$font-primary", fontSize: 14, fontWeight: 600, fill: "$info-foreground", width: "fill_container" },
      { type: "text", name: "Body", content: "Your trial ends in three days.", fontFamily: "$font-secondary", fontSize: 14, lineHeight: 1.45, fill: "$info-foreground", width: "fill_container" },
    ] },
  ] });
const item = (name, icon, active) => ({ type: "frame", name, width: "fill_container", layout: "horizontal", gap: 10, padding: [8, 12],
  alignItems: "center", cornerRadius: "$radius-md", fill: active ? "$border" : "#00000000", children: [
    { type: "icon", name: "Icon", icon, width: 16, height: 16, fill: active ? "$foreground" : "$muted-foreground" },
    { type: "text", name: "Label", content: name, fontSize: 14, fontWeight: active ? 600 : 400, fill: active ? "$foreground" : "$muted-foreground" },
  ] });
nav = Insert({ type: "frame", name: "Sidebar", ...FindEmptySpace({ width: 240, height: 200, from: alert, direction: "bottom", padding: 48 }),
  width: 240, layout: "vertical", gap: 2, padding: 12, fill: "$card", stroke: { fill: "$border", thickness: 1 }, cornerRadius: "$radius-md",
  children: [item("Home", "lucide:house", true), item("Customers", "lucide:users", false), item("Settings", "lucide:settings", false)] });
TakeScreenshot([alert, nav]);
```

## Checking both Themes

Show the system in each Theme by copying a sheet with `theme` set, and screenshot both. Every text must keep its contrast in each.

```js
const darkAlert = Copy(alert, null, { ...FindEmptySpace({ width: 420, height: 80, from: alert, padding: 48 }), theme: { mode: "dark" } });
const darkNav = Copy(nav, null, { ...FindEmptySpace({ width: 240, height: 200, from: nav, padding: 48 }), theme: { mode: "dark" } });
TakeScreenshot([darkAlert, darkNav]);
```

## Rules

- Every value in a Component comes from a Token, but for structure (a gap of 4 inside an icon button is fine as a number when it isn't on the scale's Tokens).
- One idea per card; tables of four to seven columns; reuse a Component before building a custom frame.
- When implementation follows, `GetTokens({css: true})` gives the Tokens as CSS custom properties, with each Theme under a data attribute.
