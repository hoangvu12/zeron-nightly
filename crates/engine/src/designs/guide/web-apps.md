# Web apps

Product UI for people doing work: dashboards, admin tools, editors, settings. It's functional; it leaves marketing moves (big hero type, gradients, testimonials) to landing pages.

## Before the layout

Infer what the product is, then its structure. Answer these first, in a sentence each:

- What's the one dominant region (the list, the canvas, the document, the inbox)? It gets most of the width.
- What's the primary action on this Screen?
- What density suits the work: compact for data-heavy tools, comfortable for most, airy for occasional flows?
- What's hidden until needed (advanced filters, bulk actions, settings)?

Don't reach for a sidebar, a table or a dashboard of cards by habit. Two zones (navigation and content) before adding a third (a detail panel). Whitespace separates zones better than borders; no scroll areas inside scroll areas.

## Skeletons

- **Sidebar and main**: 1440 wide; sidebar about 240–280 fixed; main `fill_container` with 32 padding and 24 gaps.
- **Top bar**: 56–64 tall, `space_between`, a bottom border, the page title or breadcrumbs on the left and actions on the right.
- **Main and side column**: main `fill_container`, side column about 320–360 fixed.
- **Card grids**: rows of equal `fill_container` cards; a grid is a column of rows, since layouts don't wrap.

```js
SetTokens({
  "bg": { type: "color", value: "#FAFAFA" },
  "panel": { type: "color", value: "#FFFFFF" },
  "border": { type: "color", value: "#E5E5E5" },
  "fg": { type: "color", value: "#0A0A0A" },
  "muted": { type: "color", value: "#737373" },
  "accent": { type: "color", value: "#4F46E5" },
  "selected": { type: "color", value: "#F0F0F0" },
});
app = Insert({ type: "frame", name: "Dashboard", x: 0, y: 0, width: 1440, height: 900, layout: "horizontal", fill: "$bg", clip: true, inProgress: true });
const navItem = (label, icon, active) => ({ type: "frame", name: label, width: "fill_container", layout: "horizontal", gap: 10, padding: [8, 12],
  cornerRadius: 6, alignItems: "center", fill: active ? "$selected" : "#00000000", children: [
    { type: "icon", name: `${label} icon`, icon, width: 16, height: 16, fill: active ? "$fg" : "$muted" },
    { type: "text", name: `${label} label`, content: label, fontSize: 14, fontWeight: active ? 600 : 400, fill: active ? "$fg" : "$muted" },
  ] });
Insert(app, { type: "frame", name: "Sidebar", width: 248, height: "fill_container", layout: "vertical", gap: 2, padding: [20, 12],
  fill: "$panel", stroke: { fill: "$border", thickness: { top: 0, right: 1, bottom: 0, left: 0 } }, children: [
    { type: "frame", name: "Brand", width: "fill_container", padding: [0, 12, 16, 12], children: [
      { type: "text", name: "Wordmark", content: "Ledgerly", fontSize: 16, fontWeight: 700, fill: "$fg" },
    ] },
    navItem("Overview", "lucide:layout-dashboard", true), navItem("Customers", "lucide:users", false), navItem("Settings", "lucide:settings", false),
  ] });
main = Insert(app, { type: "frame", name: "Main", width: "fill_container", height: "fill_container", layout: "vertical", gap: 24, padding: 32 });
Insert(main, { type: "frame", name: "Top bar", width: "fill_container", layout: "horizontal", justifyContent: "space_between", alignItems: "center", children: [
  { type: "text", name: "Page title", content: "Overview", fontSize: 24, fontWeight: 600, letterSpacing: -0.3, fill: "$fg" },
  { type: "frame", name: "New invoice", layout: "horizontal", padding: [8, 14], cornerRadius: 6, fill: "$accent", children: [
    { type: "text", name: "New invoice label", content: "New invoice", fontSize: 14, fontWeight: 600, fill: "#FFFFFF" },
  ] },
] });
```

## Showing things concretely

- Entities show what identifies and matters about them: a name, a status, the two or three facts people scan for, and the actions they take on it.
- Status reads as a small badge in a semantic colour pair, with a word, not colour alone.
- Labels beat icons where meaning matters; icon-only buttons are for universally known actions (search, close, more).
- Metrics: a muted label, a large value, and the change in a semantic colour.

```js
const metric = (label, value, change, up) => ({ type: "frame", name: label, width: "fill_container", layout: "vertical", gap: 6, padding: 20,
  cornerRadius: 10, fill: "$panel", stroke: { fill: "$border", thickness: 1 }, children: [
    { type: "text", name: `${label} label`, content: label, fontSize: 13, fill: "$muted" },
    { type: "text", name: `${label} value`, content: value, fontSize: 30, fontWeight: 600, letterSpacing: -0.5, fill: "$fg" },
    { type: "text", name: `${label} change`, content: change, fontSize: 13, fontWeight: 500, fill: up ? "#15803D" : "#B91C1C" },
  ] });
metrics = Insert(main, { type: "frame", name: "Metrics", width: "fill_container", layout: "horizontal", gap: 16,
  children: [metric("Revenue", "$48,210", "+12.4% vs last month", true), metric("Outstanding", "$6,930", "+3 invoices overdue", false),
    metric("Paid on time", "92%", "+4 points", true)] });
const bars = [42, 58, 51, 66, 72, 61, 80, 77, 85, 90, 83, 96];
Insert(main, { type: "frame", name: "Revenue chart", width: "fill_container", height: "fill_container", layout: "vertical", gap: 16, padding: 24,
  cornerRadius: 10, fill: "$panel", stroke: { fill: "$border", thickness: 1 }, children: [
    { type: "text", name: "Chart title", content: "Monthly revenue", fontSize: 15, fontWeight: 600, fill: "$fg" },
    { type: "frame", name: "Bars", width: "fill_container", height: "fill_container", layout: "horizontal", gap: 12, alignItems: "end",
      children: bars.map((v, i) => ({ type: "rectangle", name: `Bar ${i + 1}`, width: "fill_container", height: v * 3, cornerRadius: [4, 4, 0, 0],
        fill: i === bars.length - 1 ? "$accent" : "#C7D2FE" })) },
  ] });
const issues = [];
Get(app, (node, info) => { if (info.problems.length) issues.push(`${node.name}: ${info.problems}`); });
Print(issues.length ? issues : "clean");
Update(app, { inProgress: false });
TakeScreenshot(metrics);
```

## States

Design the states, not only the happy path: loading (skeleton shapes in the layout's places), empty (one sentence on what goes here and the action that fills it), error (what happened and what to do), success, and no permission. Copy the Screen for each state and change only what differs. Destructive actions confirm in a dialog; others give immediate feedback.

## Growth and breakpoints

- Design for more: a list of 3 should also work with 300 (pagination or virtual scrolling shown), a nav of 4 with 12.
- Patterns repeat across Screens: the same top bar, the same table, the same empty state.
- Progressive disclosure: advanced options behind a toggle or a menu.
- On a phone the same app is one column: the sidebar becomes a tab bar or a menu, side panels become sheets, tables become cards. Hierarchy survives every breakpoint.
