# Mobile apps

Design phone Screens 390 wide, each a Frame with `clip: true` and a height that grows with its content (`height: "fit_content"`); fix 844 only for a Screen meant to show exactly one viewport. Build the frame of the app first (status bar and tab bar), check it, then the content.

## Structure of a Screen

A vertical Frame holding, in order:

1. **Status bar**: 62 tall, the time on the left and signal and battery on the right, vertically centred, 24 side padding. Nothing else goes in it or under it.
2. **Content**: one wrapper Frame holding everything else: side padding 16–20 set once on it, `gap` 24–32 between major blocks and 12–16 inside related groups, bottom padding equal to the gap. Space comes from gaps and padding, never from empty spacer Frames.
3. **Tab bar**: the last child of the Screen's column, not `absolute`.

```js
SetTokens({
  "bg": { type: "color", value: "#F6F6F4" },
  "surface": { type: "color", value: "#FFFFFF" },
  "fg": { type: "color", value: "#111111" },
  "muted": { type: "color", value: "#76766F" },
  "accent": { type: "color", value: "#16A34A" },
  "accent-soft": { type: "color", value: "#DCFCE7" },
});
phone = Insert({ type: "frame", name: "Home", x: 0, y: 0, width: 390, height: "fit_content", layout: "vertical", fill: "$bg", clip: true, inProgress: true });
Insert(phone, { type: "frame", name: "Status bar", width: "fill_container", height: 62, layout: "horizontal", padding: [0, 24],
  justifyContent: "space_between", alignItems: "center", children: [
    { type: "text", name: "Time", content: "9:41", fontSize: 16, fontWeight: 600, fill: "$fg" },
    { type: "frame", name: "Indicators", layout: "horizontal", gap: 6, alignItems: "center", children: [
      { type: "rectangle", name: "Signal", width: 18, height: 10, cornerRadius: 2, fill: "$fg" },
      { type: "rectangle", name: "Battery", width: 26, height: 12, cornerRadius: 3, fill: "$fg" },
    ] },
  ] });
content = Insert(phone, { type: "frame", name: "Content", width: "fill_container", layout: "vertical", gap: 28, padding: [8, 20, 28, 20] });
```

## Tab bar

For three to five destinations, a floating capsule reads as modern and keeps content visible around it:

- Inset about 16 from the sides and 12 from the bottom (a wrapper Frame with that padding).
- About 56 tall, radius half the height, about 6 inside padding, items sharing the width.
- A translucent surface (about 70% opaque) with a soft shadow, and a background blur if content shows through.
- Each item: a 22 icon over a 10 label in sentence case.
- The selected item: a filled icon in the accent on a soft accent pill; the others outlined icons in a muted colour. Icon sets with filled and outline pairs make this easy: Phosphor (`ph:house`, `ph:house-fill`), Material Symbols (`material-symbols:home-outline`, `material-symbols:home`). Lucide has no filled icons.

```js
const tab = (label, icon, selected) => ({ type: "frame", name: `Tab ${label}`, width: "fill_container", height: "fill_container",
  layout: "vertical", gap: 2, justifyContent: "center", alignItems: "center", cornerRadius: 999, fill: selected ? "$accent-soft" : "#00000000",
  children: [
    { type: "icon", name: `${label} icon`, icon: selected ? `${icon}-fill` : icon, width: 22, height: 22, fill: selected ? "$accent" : "$muted" },
    { type: "text", name: `${label} label`, content: label, fontSize: 10, fontWeight: selected ? 600 : 500, fill: selected ? "$accent" : "$muted" },
  ] });
Insert(phone, { type: "frame", name: "Tab bar", width: "fill_container", padding: [0, 16, 12, 16], children: [
  { type: "frame", name: "Capsule", width: "fill_container", height: 56, padding: 6, layout: "horizontal", cornerRadius: 28,
    fill: "#FFFFFFB3", effect: [{ type: "shadow", offset: { x: 0, y: 6 }, blur: 20, color: "#0000001F" }, { type: "background_blur", radius: 12 }],
    children: [tab("Home", "ph:house", true), tab("Search", "ph:magnifying-glass", false), tab("Saved", "ph:heart", false), tab("Profile", "ph:user", false)] },
] });
TakeScreenshot(phone);
```

## Content

- One main intent per Screen. The title is the same size on every Screen (28–34, bold); main actions sit in the lower half, within thumb reach.
- Touch targets are at least 44 × 44, even when the visible icon is smaller: pad the Frame around it.
- Lists are full-width rows (56–72 tall) with a leading icon or avatar, a title and a muted subtitle, and a trailing value or chevron. Dividers are optional when the spacing groups well.
- Cards span the content width (`fill_container`); horizontal carousels show a peek of the next card.
- Text: body 15–17, never under 12; labels in sentence case.

```js
const row = (title, detail, amount) => ({ type: "frame", name: title, width: "fill_container", height: 64, layout: "horizontal", gap: 12,
  padding: [0, 16], alignItems: "center", children: [
    { type: "ellipse", name: `${title} avatar`, width: 40, height: 40, fill: "$accent-soft" },
    { type: "frame", name: `${title} text`, width: "fill_container", layout: "vertical", gap: 2, children: [
      { type: "text", name: `${title} title`, content: title, fontSize: 16, fontWeight: 500, fill: "$fg" },
      { type: "text", name: `${title} detail`, content: detail, fontSize: 13, fill: "$muted" },
    ] },
    { type: "text", name: `${title} amount`, content: amount, fontSize: 16, fontWeight: 600, fill: "$fg" },
  ] });
Insert(content, { type: "text", name: "Title", content: "Groceries", fontSize: 32, fontWeight: 700, letterSpacing: -0.5, fill: "$fg" });
Insert(content, { type: "frame", name: "Recent", width: "fill_container", layout: "vertical", cornerRadius: 20, fill: "$surface", padding: [4, 0],
  children: [row("Greenmarket", "Today, 9:12", "$42.10"), row("Corner Bakery", "Yesterday", "$8.40"), row("FreshBox", "Mon", "$61.00")] });
const issues = [];
Get(phone, (node, info) => {
  if (info.problems.length) issues.push(`${node.name}: ${info.problems}`);
  if (node.type === "text" && node.fontSize < 12 && !node.name.endsWith("label")) issues.push(`${node.name}: under 12 px`);
});
Print(issues.length ? issues : "clean");
Update(phone, { inProgress: false });
TakeScreenshot(content);
```

## Several Screens

Place each new Screen with `FindEmptySpace({width: 390, height: 844, from: previousScreen, padding: 80})`, in the order a person moves through them. Make the status bar and tab bar Components once the second Screen needs them, and give each Screen its own Instance with the right tab selected.
