# Tables

A table is Frames all the way down, so its columns line up by construction:

- **Table**: a vertical Frame, `width: "fill_container"` (or fixed), with a border and a radius from the Tokens and `clip: true`.
- **Row**: a horizontal Frame, `width: "fill_container"`, with a bottom border (a stroke with only `bottom` set).
- **Cell**: a Frame in the row with the column's width and `height: "fill_container"`, its padding, and its alignment.
- **Content**: a text, a badge, an avatar, an icon button, inside the cell. Text never sits directly in a row.

The header row is built the same way, with the same column widths, its text muted or bold and smaller.

## Column widths

Fix every column but one, and let that one fill:

- Name or ID: 200–250.
- Email, URL, description: `fill_container`.
- Status: 100–120.
- Dates: 120–150.
- Numbers: 80–120, right-aligned (`justifyContent: "end"`), in a face with even figures.
- Actions: 64–100, right-aligned.

Four to seven columns. More, and the table needs a different design (a detail panel, or a choice of columns).

## Build it from data

Describe the columns once, then build the header and each row from the same description. Use plausible data: real-looking names, varied lengths, a long one to test truncation, numbers with realistic spread.

```js
SetTokens({
  "panel": { type: "color", value: "#FFFFFF" },
  "line": { type: "color", value: "#EAEAEA" },
  "head": { type: "color", value: "#F7F7F7" },
  "fg": { type: "color", value: "#111111" },
  "muted": { type: "color", value: "#6E6E6E" },
});
columns = [
  { key: "name", title: "Customer", width: 220 },
  { key: "email", title: "Email", width: "fill_container" },
  { key: "status", title: "Status", width: 120 },
  { key: "joined", title: "Joined", width: 130 },
  { key: "total", title: "Total", width: 110, end: true },
];
rows = [
  { name: "Amara Okafor", email: "amara@lumenhq.com", status: "Active", joined: "Mar 3, 2025", total: "$12,480" },
  { name: "Jonas Lindqvist", email: "jonas.lindqvist@northpeak-logistics.se", status: "Trial", joined: "Apr 18, 2025", total: "$0" },
  { name: "Priya Raman", email: "priya@quill.dev", status: "Past due", joined: "Jan 9, 2024", total: "$3,215" },
  { name: "Mateo Ruiz", email: "mateo@ruiz.studio", status: "Active", joined: "Jun 27, 2025", total: "$880" },
];
```

```js
const badgeColors = { "Active": ["#DCFCE7", "#166534"], "Trial": ["#E0E7FF", "#3730A3"], "Past due": ["#FEE2E2", "#991B1B"] };
const cell = (column, child) => ({ type: "frame", name: `${column.title} cell`, width: column.width, height: "fill_container",
  layout: "horizontal", padding: [0, 16], alignItems: "center", justifyContent: column.end ? "end" : "start", children: [child] });
const text = (content, header) => ({ type: "text", name: "Value", content, fontSize: header ? 12 : 14, fontWeight: header ? 600 : 400,
  fill: header ? "$muted" : "$fg" });
const badge = (status) => ({ type: "frame", name: "Badge", padding: [2, 8], cornerRadius: 999, fill: badgeColors[status][0], children: [
  { type: "text", name: "Badge label", content: status, fontSize: 12, fontWeight: 500, fill: badgeColors[status][1] }] });
const row = (name, children, fill) => ({ type: "frame", name, width: "fill_container", height: 52, layout: "horizontal", fill,
  stroke: { fill: "$line", thickness: { top: 0, right: 0, bottom: 1, left: 0 } }, children });
table = Insert({ type: "frame", name: "Customers table", x: 0, y: 0, width: 960, layout: "vertical", cornerRadius: 10, clip: true,
  fill: "$panel", stroke: { fill: "$line", thickness: 1 }, children: [
    row("Header", columns.map((c) => cell(c, text(c.title, true))), "$head"),
    ...rows.map((r) => row(r.name, columns.map((c) => cell(c, c.key === "status" ? badge(r.status) : text(r[c.key], false))), "$panel")),
  ] });
const clipped = [];
Get(table, (node, info) => { if (info.problems.length) clipped.push(`${node.name}: ${info.problems}`); });
Print(clipped.length ? clipped : "clean");
TakeScreenshot(table);
```

A cell neither wraps nor cuts off its text: a value longer than its column runs into the next one, and only the table's own `clip` reports it. Compare each text's bounds with its cell's in a visitor, then widen the column, shorten the value, or give the text `width: "fill_container"` so it wraps.

## Around the table

- **Toolbar** above: search on the left, filters beside it, the primary action on the right.
- **Footer** below: a count ("Showing 1–20 of 248") on the left, pagination on the right.
- Row actions sit in the last column as an icon button (`lucide:ellipsis`), or appear on the selected row.
- Selected rows get a quiet fill; selection checkboxes go in a narrow first column.
- Zebra stripes only for very wide or dense tables; borders between rows usually do the job.

```js
const spot = FindEmptySpace({ width: 960, height: 36, from: table, direction: "top", padding: 16 });
toolbar = Insert({ type: "frame", name: "Toolbar", ...spot, width: 960, layout: "horizontal", justifyContent: "space_between", alignItems: "center", children: [
  { type: "frame", name: "Search", width: 280, height: 36, layout: "horizontal", gap: 8, padding: [0, 12], alignItems: "center", cornerRadius: 8,
    stroke: { fill: "$line", thickness: 1 }, children: [
      { type: "icon", name: "Search icon", icon: "lucide:search", width: 16, height: 16, fill: "$muted" },
      { type: "text", name: "Placeholder", content: "Search customers", fontSize: 14, fill: "$muted" },
    ] },
  { type: "frame", name: "More", width: 36, height: 36, cornerRadius: 8, layout: "horizontal", justifyContent: "center", alignItems: "center",
    stroke: { fill: "$line", thickness: 1 }, children: [{ type: "icon", name: "More icon", icon: "lucide:ellipsis", width: 16, height: 16, fill: "$fg" }] },
] });
TakeScreenshot(toolbar);
```

## Large tables

Build a long table across calls: the header and the first rows in one call, more rows in the next (`Insert(table, row(...))` with the helpers defined again, since functions don't carry between calls). A table in a Screen shows a page of rows (10–20) and the pagination, never hundreds.

On a phone, a table becomes a list of cards: the name as the title, two or three key fields below, the status as a badge.
