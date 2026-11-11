# Landing pages

A landing page has one conversion goal (sign up, book a demo, download), and every section either moves the reader toward it or goes. Design at 1440 wide, as one Screen whose height grows with its sections (`height: "fit_content"`), built a section per call.

## Story

Write the copy before any visuals, in this arc:

1. **What it is**: the hero. One headline, one sentence of support, one call to action.
2. **Why it works**: three or four benefits, or how it works in steps.
3. **Proof**: numbers, logos (as wordmarks in type), quotes from real-sounding people with names and roles.
4. **The ask**: the call to action again, with any objection answered (price, trial, cancellation).

Headlines, strongest first: the change in the reader's life, a concrete result, a benefit, a feature. Lead with the strongest you can honestly claim, in eight words or fewer.

## Hero

- The headline is the largest thing on the page (56–88 on desktop), in the display face, tightened slightly (`letterSpacing` -1 to -2, `lineHeight` about 1.05).
- The call to action is the loudest element after it: the only filled accent button above the fold. A secondary action, if any, is a text link or outlined button.
- Say the offer, its value and the next step within the first screenful (about 800 px).
- You can't generate images. Use the person's images, or build the hero from type and colour: a bold colour block, an oversized number, a product mock made of Frames. Where an image belongs and none is given, reserve the area with a quiet fill and a small label saying what goes there.
- Text over an image always gets contrast help: a dark scrim, a gradient from the side the text is on, or a solid band. Images keep their proportions (`mode: "cover"`).

```js
SetTokens({
  "bg": { type: "color", value: "#F7F5F0" },
  "ink": { type: "color", value: "#1A1814" },
  "ink-muted": { type: "color", value: "#5F5A50" },
  "accent": { type: "color", value: "#C2410C" },
  "on-accent": { type: "color", value: "#FFFFFF" },
  "band": { type: "color", value: "#1A1814" },
  "font-display": { type: "string", value: "Instrument Serif" },
  "font-text": { type: "string", value: "Geist" },
});
```

The photo here is one the person supplied, copied into the Design's `images/` folder.

```js
page = Insert({ type: "frame", name: "Landing", x: 0, y: 0, width: 1440, height: "fit_content", layout: "vertical", fill: "$bg", clip: true, inProgress: true });
hero = Insert(page, { type: "frame", name: "Hero", width: "fill_container", layout: "horizontal", gap: 64, padding: [96, 120], alignItems: "center", children: [
  { type: "frame", name: "Copy", width: "fill_container", layout: "vertical", gap: 24, children: [
    { type: "text", name: "Headline", content: "Cook dinner in twenty minutes, every night", fontFamily: "$font-display", fontSize: 76, lineHeight: 1.02, letterSpacing: -1.5, fill: "$ink", width: "fill_container" },
    { type: "text", name: "Support", content: "Recipes planned around what is already in your kitchen, with one shopping list for the week.", fontFamily: "$font-text", fontSize: 19, lineHeight: 1.5, fill: "$ink-muted", width: "fill_container" },
    { type: "frame", name: "Actions", layout: "horizontal", gap: 20, alignItems: "center", children: [
      { type: "frame", name: "CTA", layout: "horizontal", gap: 8, padding: [14, 24], cornerRadius: 999, fill: "$accent", alignItems: "center", children: [
        { type: "text", name: "CTA label", content: "Plan my week", fontFamily: "$font-text", fontSize: 16, fontWeight: 600, fill: "$on-accent" },
        { type: "icon", name: "CTA arrow", icon: "lucide:arrow-right", width: 18, height: 18, fill: "$on-accent" },
      ] },
      { type: "text", name: "Reassurance", content: "Free for 14 days", fontFamily: "$font-text", fontSize: 15, fill: "$ink-muted" },
    ] },
  ] },
  { type: "frame", name: "Photo", width: 520, height: 600, cornerRadius: 24, clip: true, layout: "vertical", justifyContent: "end", padding: 24,
    fill: [{ type: "image", url: "images/photo.png", mode: "cover" }, { type: "gradient", rotation: 0, colors: [{ color: "#000000AA", position: 0 }, { color: "#00000000", position: 0.5 }] }],
    children: [{ type: "text", name: "Caption", content: "Tuesday: miso salmon, 18 minutes", fontFamily: "$font-text", fontSize: 15, fontWeight: 500, fill: "#FFFFFF" }] },
] });
TakeScreenshot(hero);
```

## Sections

- Each section chooses one alignment, left or centred, and keeps it.
- Alternate text-led and visual sections, and vary their structure: a three-column benefit row, then a wide split, then a dark band. A page of identical card grids reads as a template.
- Dark bands lend weight (proof, the final ask); light ones explain.
- Keep a consistent section rhythm: the same vertical padding (96–128) and the same content width.
- Benefits are short: a bold phrase and one sentence each, with an icon only if it carries meaning.

```js
const stat = (value, label) => ({ type: "frame", name: label, width: "fill_container", layout: "vertical", gap: 8, children: [
  { type: "text", name: `${label} value`, content: value, fontFamily: "$font-display", fontSize: 64, letterSpacing: -1, fill: "#FFFFFF" },
  { type: "text", name: `${label} label`, content: label, fontFamily: "$font-text", fontSize: 16, lineHeight: 1.4, fill: "#A8A29E", width: "fill_container" },
] });
proof = Insert(page, { type: "frame", name: "Proof", width: "fill_container", layout: "horizontal", gap: 48, padding: [96, 120], fill: "$band",
  children: [stat("2.1M", "dinners planned last year"), stat("31%", "less food thrown away"), stat("4.8", "average rating from 40k reviews")] });
TakeScreenshot(proof);
```

```js
Insert(page, { type: "frame", name: "Final ask", width: "fill_container", layout: "vertical", gap: 24, padding: [128, 120], alignItems: "center", children: [
  { type: "text", name: "Ask", content: "Your first week is on us", fontFamily: "$font-display", fontSize: 56, textAlign: "center", fill: "$ink" },
  { type: "frame", name: "Final CTA", layout: "horizontal", padding: [14, 24], cornerRadius: 999, fill: "$accent", children: [
    { type: "text", name: "Final CTA label", content: "Plan my week", fontFamily: "$font-text", fontSize: 16, fontWeight: 600, fill: "$on-accent" },
  ] },
] });
const problems = [];
Get(page, (node, info) => { if (info.problems.length) problems.push(`${node.name}: ${info.problems}`); });
Print(problems.length ? problems : "clean");
Update(page, { inProgress: false });
```

## Checks

- Squint at a screenshot of each section: the headline and the call to action should be the first two things you see.
- The accent appears only on actions and one or two emphasised words.
- Every section passes the cut test: if removing it loses nothing, remove it.
- For a mobile version, copy the page to a 390-wide Screen, stack every row into a column, and drop the headline to 40–48.
