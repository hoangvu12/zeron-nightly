# Tailwind

Implementing a Design with Tailwind CSS (v4). Read the `code` topic guide first for how to read the Design; this guide is how to write what you read as Tailwind.

## Setup

- One `@import "tailwindcss";` at the top of the main stylesheet; no hand-written resets (Tailwind's preflight is one).
- The Design's Tokens go in as CSS custom properties: `GetTokens({css: true})` writes them, the default Theme on `:root` and the others under data attributes (`[data-mode="dark"]`). Set that attribute on the element (often `<html>`) to switch Themes.
- Fonts load with a `<link>` to Google Fonts or the framework's font loader, not `@import url()` inside the CSS (it must come first and blocks rendering).
- A font stack containing commas doesn't survive being passed through a variable into an arbitrary value. Give each family a class of its own in `@layer base` or `@theme` (`--font-display: "Instrument Serif", serif;` then `font-display`), or with a framework font loader use the variable it gives you directly.
- `html`, `body` and the app root are `h-full`; the root of each Screen is `h-full w-full`. The Design's Screen width is the layout it was designed at, not a width to fix: only things that really are fixed (a sidebar, an icon button) keep their pixel width.

```js
SetTokens({
  "surface": { type: "color", value: [{ value: "#FFFFFF", theme: { mode: "light" } }, { value: "#0B0B0F", theme: { mode: "dark" } }] },
  "text": { type: "color", value: [{ value: "#0B0B0F", theme: { mode: "light" } }, { value: "#F4F4F5", theme: { mode: "dark" } }] },
  "brand": { type: "color", value: "#7C3AED" },
  "radius": { type: "number", value: 12 },
});
Print(GetTokens({ css: true }));
```

## Styling

- Utilities only, no inline `style`. Arbitrary values hit the Design's numbers exactly: `w-[280px]`, `gap-[18px]`, `text-[15px]`, `leading-[1.45]`, `tracking-[-0.5px]`.
- Colours through the Token variables, never hex: `bg-[var(--brand)]`, `text-[var(--text)]`, `border-[var(--border)]`. In v4 the shorthand `bg-(--brand)` works too.
- Radius and other number Tokens the same way: `rounded-[calc(var(--radius)*1px)]`, or define them with units in your own `@theme`.
- States with variants: `hover:`, `focus-visible:`, `active:`, `disabled:`, `aria-selected:`, and `dark:` only if the project's dark mode uses a class; with Token Themes, the variables switch on their own.
- SVG icons and paths: `fill-[var(--x)]`, `stroke-[var(--x)]`, sized with `size-4` or `size-[18px]`.

## Layout

| Design | Tailwind |
| --- | --- |
| `layout: "horizontal"` / `"vertical"` | `flex flex-row` / `flex flex-col` |
| `gap: 12`, `padding: [8, 16]` | `gap-[12px] py-[8px] px-[16px]` |
| `justifyContent: "space_between"` | `justify-between` |
| `alignItems: "center"` | `items-center` |
| `width: "fill_container"` in a row | `flex-1 min-w-0` |
| `width: "fill_container"` in a column | `self-stretch` (or `w-full`) |
| `height: "fill_container"` in a column | `flex-1 min-h-0`, with every parent up the chain given a height |
| `fit_content` | `w-fit` / `h-fit`, or nothing |
| `absolute: true`, `x`, `y` | `absolute left-[x] top-[y]` in a `relative` parent |
| `clip: true` | `overflow-hidden` |

A region that scrolls in the real app (a list in a fixed-height panel) is `flex-1 min-h-0 overflow-auto`; the Design shows it unscrolled.

## Starting from an export

`Export([screen], {format: "html", path, css: "tailwind"})` writes a Screen as Tailwind-classed HTML: exact values as arbitrary classes, Token colours as `var()` references. Use it to read off structure and values, then rebuild it in the project's components; flatten the wrappers it doesn't need.

```js
const card = Insert({ type: "frame", name: "Plan card", x: 0, y: 0, width: 320, layout: "vertical", gap: 16, padding: 24,
  cornerRadius: "$radius", fill: "$surface", stroke: { fill: "#E4E4E7", thickness: 1 }, children: [
    { type: "text", name: "Plan", content: "Team", fontSize: 15, fontWeight: 600, fill: "$text" },
    { type: "text", name: "Price", content: "$24 per seat", fontSize: 28, fontWeight: 700, letterSpacing: -0.5, fill: "$text" },
    { type: "frame", name: "Choose", width: "fill_container", layout: "horizontal", justifyContent: "center", padding: [10, 16], cornerRadius: 999, fill: "$brand",
      children: [{ type: "text", name: "Choose label", content: "Choose Team", fontSize: 14, fontWeight: 600, fill: "#FFFFFF" }] },
  ] });
Print(Export([card], { format: "html", path: "handoff/plan-card.html", css: "tailwind", scaffold: false }));
```

Written by hand in the project, the same card is:

```html
<div class="flex w-[320px] flex-col gap-[16px] rounded-[12px] bg-[var(--surface)] p-[24px] shadow-[inset_0_0_0_1px_#E4E4E7]">
  <p class="text-[15px] font-semibold text-[var(--text)]">Team</p>
  <p class="text-[28px] font-bold tracking-[-0.5px] text-[var(--text)]">$24 per seat</p>
  <button class="flex justify-center self-stretch rounded-full bg-[var(--brand)] px-[16px] py-[10px] text-[14px] font-semibold text-white">Choose Team</button>
</div>
```
