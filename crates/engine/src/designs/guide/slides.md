# Slides

Each slide is a Screen of 1920 × 1080 with `clip: true`, its content kept at least 100 from every edge (`padding: 100` or more). A deck is a row of slides placed in order with `FindEmptySpace`, each named by its number and point (`03 Why now`). Build a slide per call, or two or three when they share a layout.

## Priorities

Clarity first, then readability from the back of a room, then hierarchy, then simplicity.

- **One message per slide.** The title states the takeaway as a sentence ("Churn halved after onboarding changed"), not a topic ("Churn").
- **Short phrases, not paragraphs.** When it doesn't fit, split the slide or cut words. Never shrink the type to make it fit.
- **Type for projection.** Body 28–32 (never under 24), titles 40 and up, line height 1.1–1.2 for display sizes, two families at most. Scale a brand's type up rather than using its web sizes.
- **Colour.** Two or three colours plus neutrals, the accent for the one thing to notice, strong contrast, and pairs that stay distinct for colour-blind viewers (blue and orange rather than red and green).
- **Text-only slides go bold**: oversized type, asymmetric placement, one accent word.

```js
SetTokens({
  "paper": { type: "color", value: "#F4F1EA" },
  "ink": { type: "color", value: "#141414" },
  "quiet": { type: "color", value: "#6B6760" },
  "accent": { type: "color", value: "#2F5BEA" },
  "display": { type: "string", value: "Instrument Serif" },
  "text": { type: "string", value: "Geist" },
});
const slide = (name, props) => {
  const spot = FindEmptySpace({ width: 1920, height: 1080, padding: 120 });
  return Insert({ type: "frame", name, ...spot, width: 1920, height: 1080, fill: "$paper", clip: true, padding: 120, ...props });
};
cover = slide("01 Cover", { layout: "vertical", justifyContent: "end", gap: 32, children: [
  { type: "text", name: "Kicker", content: "Q3 review", fontFamily: "$text", fontSize: 28, fontWeight: 500, fill: "$quiet" },
  { type: "text", name: "Title", content: "We halved churn by fixing the first week", fontFamily: "$display", fontSize: 120, lineHeight: 1.0, letterSpacing: -2, fill: "$ink", width: 1400 },
] });
TakeScreenshot(cover);
```

## Layouts

- **Cover**: a title of 48–72 (more for a type-led cover), two lines at most.
- **Section break**: a small muted label over a 48–56 title.
- **Statement**: one sentence of 36–48, at most two lines, lots of space.
- **Quote**: 28–36, at most three lines, attribution at 20–24 below.
- **Concept and visual**: a 50/50 split, title 36–40 and at most four lines of body on one side, the visual on the other, at least 40 apart, vertically centred. Or a full-bleed image under a dark scrim with a 40–56 title.
- **Pillars**: three or four equal columns, top-aligned, each a visual or icon, a 28 label and a 20 description, 30–50 apart.
- **Comparison**: two columns 40–60 apart, or before and after with "before" muted.
- **KPIs**: hero numbers at 120–200 for one, 80–120 for two, 64–80 for three, each with a short label.
- **Process**: three to five steps along one line.
- **2 × 2**: four equal cards, 20–30 apart.
- **List**: a 40 title and three to five items at 28.
- **Closing**: 48–56 and the contact details or the ask.

Helpers like `slide` don't carry from one call to the next (globals hold only JSON), so each snippet defines the ones it uses.

```js
const slide = (name, props) => {
  const spot = FindEmptySpace({ width: 1920, height: 1080, padding: 120 });
  return Insert({ type: "frame", name, ...spot, width: 1920, height: 1080, fill: "$paper", clip: true, padding: 120, ...props });
};
const kpi = (value, label) => ({ type: "frame", name: label, width: "fill_container", layout: "vertical", gap: 16, children: [
  { type: "text", name: `${label} value`, content: value, fontFamily: "$display", fontSize: 180, lineHeight: 1.0, letterSpacing: -4, fill: "$accent" },
  { type: "text", name: `${label} label`, content: label, fontFamily: "$text", fontSize: 30, lineHeight: 1.3, fill: "$ink", width: "fill_container" },
] });
results = slide("02 Results", { layout: "vertical", gap: 96, children: [
  { type: "text", name: "Takeaway", content: "Better onboarding moved both numbers that matter", fontFamily: "$display", fontSize: 64, lineHeight: 1.05, fill: "$ink", width: "fill_container" },
  { type: "frame", name: "KPIs", width: "fill_container", layout: "horizontal", gap: 120, children: [kpi("-48%", "churn in the first 30 days"), kpi("2.3×", "teams inviting a colleague in week one")] },
] });
TakeScreenshot(results);
```

## Charts on slides

One insight per chart. Make the chart about 60% of the slide's height, highlight the bar or segment that carries the point in the accent and the rest in a quiet tone, and put the insight in a bold one-line title. Bars are Frames in a horizontal layout aligned to the bottom; labels sit in the layout under them.

```js
const slide = (name, props) => {
  const spot = FindEmptySpace({ width: 1920, height: 1080, padding: 120 });
  return Insert({ type: "frame", name, ...spot, width: 1920, height: 1080, fill: "$paper", clip: true, padding: 120, ...props });
};
const months = [["Apr", 9.1], ["May", 8.7], ["Jun", 8.9], ["Jul", 6.2], ["Aug", 5.1], ["Sep", 4.7]];
const column = ([month, value], i) => ({ type: "frame", name: month, width: "fill_container", layout: "vertical", gap: 16, alignItems: "center", children: [
  { type: "text", name: `${month} value`, content: `${value}%`, fontFamily: "$text", fontSize: 26, fontWeight: 600, fill: i >= 3 ? "$accent" : "$quiet" },
  { type: "rectangle", name: `${month} bar`, width: "fill_container", height: value * 56, cornerRadius: [8, 8, 0, 0], fill: i >= 3 ? "$accent" : "#D9D4C9" },
  { type: "text", name: `${month} label`, content: month, fontFamily: "$text", fontSize: 24, fill: "$quiet" },
] });
chart = slide("03 Churn", { layout: "vertical", gap: 64, children: [
  { type: "text", name: "Insight", content: "Churn fell from July, when the new first week shipped", fontFamily: "$display", fontSize: 56, fill: "$ink", width: "fill_container" },
  { type: "frame", name: "Chart", width: "fill_container", height: "fill_container", layout: "horizontal", gap: 48, alignItems: "end", children: months.map(column) },
] });
const issues = [];
Get(chart, (node, info) => { if (info.problems.length) issues.push(`${node.name}: ${info.problems}`); });
Print(issues.length ? issues : "clean");
TakeScreenshot(chart);
```

## Matching the context

Corporate decks are structured and restrained; startup pitches minimal and bold; marketing decks lead with benefits; internal reviews can be denser; keynotes lean on visuals and single words. Ask yourself who's in the room, and match it.
